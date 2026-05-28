// LocalVault implementation — vault-core feature.
//
// Implements `Vault` for `LocalVault` declared in `interfaces.rs`.
//
// Invariants enforced here:
//   - raw secret bytes appear ONLY in substitute(),
//     written to req.headers["Authorization"], zeroized on drop via Zeroizing<Vec<u8>>.
//   - OAuth-only at v1.
//   - 4-check sequence in substitute(): revoked → origin → scopes → ttl.
//   - every vault event appends a typed audit entry via ManifestWriter::append_audit.

use crate::error::{LoomError, LoomErrorCode};
use crate::manifest_writer::manifest_writer::{AuditKind, SessionId};
use crate::vault::vault::{
    AddCredentialOpts, AddCredentialReceipt, CredentialType, DeleteSecretOutcome, Grant, GrantId,
    GrantOpts, GrantSnapshot, LocalVault, NetRequest, RevokeReason, Vault, OAUTH_PROVIDER_ALLOWLIST,
};
use loom_keychain::{KeychainAccess, KeychainError, KeychainErrorKind};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

/// Translate a backend-layer `KeychainError` into a vault-layer `LoomError`
/// per the A-W5.2 inline error-translation table:
///
/// | `KeychainErrorKind`     | `LoomErrorCode`            |
/// |-------------------------|----------------------------|
/// | `NotFound`              | `VaultUnknownLabel`        |
/// | `Denied`                | `VaultPermissionDenied`    |
/// | `Unavailable`           | `VaultBackendUnavailable`  |
/// | `TimedOut`              | `VaultBackendTimeout`      |
/// | `NonInteractivePrompt`  | `VaultNonInteractivePrompt`|
/// | `Internal`              | `VaultInternal`            |
///
/// `Internal` carries the SHA-256-hashed identifier of the original
/// error message in `LoomError.context.internal_hash` so support can
/// correlate to the daemon's `tracing::error!` log (A-W6.3) — the
/// original message itself is never persisted in any session manifest.
fn from_keychain_err(err: KeychainError) -> LoomError {
    let code = match err.kind() {
        KeychainErrorKind::NotFound => LoomErrorCode::VaultUnknownLabel,
        KeychainErrorKind::Denied => LoomErrorCode::VaultPermissionDenied,
        KeychainErrorKind::Unavailable => LoomErrorCode::VaultBackendUnavailable,
        KeychainErrorKind::TimedOut => LoomErrorCode::VaultBackendTimeout,
        KeychainErrorKind::NonInteractivePrompt => LoomErrorCode::VaultNonInteractivePrompt,
        KeychainErrorKind::Internal => LoomErrorCode::VaultInternal,
    };
    let internal_hash = err.internal_hash().map(str::to_owned);
    let mut out = LoomError::new(code, err.to_string());
    if let Some(hash) = internal_hash {
        out = out.with_context(serde_json::json!({ "internal_hash": hash }));
    }
    out
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn new_grant_id() -> GrantId {
    GrantId(ulid::Ulid::new().to_string())
}

/// Canonical audit payload for vault lifecycle events.
/// Serialized to JCS bytes.
/// NEVER contains raw secret bytes.
#[derive(Serialize)]
struct VaultAuditPayload<'a> {
    credential_label: &'a str,
    grant_id: &'a str,
    origin: &'a str,
    requested_scopes: &'a [String],
    result: &'static str,
    triggering_action_id: Option<u64>,
    ts_tick: u64,
}

fn audit_bytes(payload: &VaultAuditPayload<'_>) -> Vec<u8> {
    // JCS (sorted keys) required for hash-chain integrity.
    serde_jcs::to_string(payload)
        .unwrap_or_else(|_| serde_json::to_string(payload).unwrap_or_default())
        .into_bytes()
}

// ─── v0.9.4 credential-lifecycle audit payloads (W5.3) ────────────────

/// Three-tier size category for stored credentials (D24). Eliminates
/// the exact-byte side-channel that `byte_count` would expose in the
/// hash-chained audit manifest.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SizeBucket {
    /// ≤ 256 bytes (typical for OAuth bearer tokens, API keys).
    Small,
    /// ≤ 4096 bytes (long tokens, refresh-token bundles).
    Medium,
    /// > 4096 bytes (large session cookies, multi-part credentials).
    Large,
}

fn size_bucket(byte_count: usize) -> SizeBucket {
    if byte_count <= 256 {
        SizeBucket::Small
    } else if byte_count <= 4096 {
        SizeBucket::Medium
    } else {
        SizeBucket::Large
    }
}

/// Wire-stable category of a `KeychainError` for audit-entry payloads
/// (D30 typed-reason requirement). Mirrors `KeychainErrorKind` but is
/// serialised as snake_case strings inside `SecretAuditPayload::*Failed`
/// variants — NOT free-form messages, which could leak third-party
/// error text into the persistent hash chain.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SecretReason {
    NotFound,
    Denied,
    Unavailable,
    TimedOut,
    NonInteractivePrompt,
    Internal,
}

fn secret_reason(err: &KeychainError) -> SecretReason {
    match err.kind() {
        KeychainErrorKind::NotFound => SecretReason::NotFound,
        KeychainErrorKind::Denied => SecretReason::Denied,
        KeychainErrorKind::Unavailable => SecretReason::Unavailable,
        KeychainErrorKind::TimedOut => SecretReason::TimedOut,
        KeychainErrorKind::NonInteractivePrompt => SecretReason::NonInteractivePrompt,
        KeychainErrorKind::Internal => SecretReason::Internal,
    }
}

/// Discriminator for the pre-op `SecretOpPending` audit (D8 G5b).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SecretOp {
    Set,
    Get,
    Delete,
    List,
}

/// Tagged audit payload for credential lifecycle. JCS-encoded via
/// `secret_audit_bytes` and embedded in `ManifestEntry::AuditEntry::canonical_bytes`.
///
/// **Never carries raw secret bytes.** `Stored` reports a 3-tier
/// `size_bucket` (D24) rather than `byte_count` to eliminate the
/// exact-size side-channel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
#[allow(dead_code)] // OwnerChanged is emitted by Linux-backend audit wiring (W5 follow-up)
enum SecretAuditPayload<'a> {
    OpPending {
        label: &'a str,
        op: SecretOp,
    },
    Stored {
        label: &'a str,
        size_bucket: SizeBucket,
        replaced: bool,
    },
    Fetched {
        label: &'a str,
    },
    Deleted {
        label: &'a str,
        cascade_revoked_grants: u32,
    },
    Listed {
        count: u32,
        service_id: &'a str,
    },
    StoreFailed {
        label: &'a str,
        reason: SecretReason,
        internal_hash: Option<&'a str>,
    },
    DeleteFailed {
        label: &'a str,
        reason: SecretReason,
        internal_hash: Option<&'a str>,
    },
    FetchFailed {
        label: &'a str,
        reason: SecretReason,
        internal_hash: Option<&'a str>,
    },
    PromptBlocked {
        label: &'a str,
        op: SecretOp,
    },
    OwnerChanged {
        pinned: &'a str,
        current: &'a str,
    },
}

fn secret_audit_bytes(payload: &SecretAuditPayload<'_>) -> Vec<u8> {
    serde_jcs::to_string(payload)
        .unwrap_or_else(|_| serde_json::to_string(payload).unwrap_or_default())
        .into_bytes()
}

impl Vault for LocalVault {
    fn grant(&self, session: SessionId, opts: GrantOpts) -> Result<GrantId, LoomError> {
        // Step 1: OAuth-only at v1
        if opts.credential_type != CredentialType::OAuth {
            return Err(
                LoomError::new(LoomErrorCode::VaultRejection, "vault-oauth-only").with_context(
                    serde_json::json!({
                        "code": "vault_credential_type_unsupported",
                        "details": { "allowed_types": ["oauth2_authorization_code_pkce"] }
                    }),
                ),
            );
        }

        // Step 2: Threat model acknowledgement gate
        if !opts.threat_model_acknowledged {
            return Err(LoomError::new(
                LoomErrorCode::VaultRejection,
                "vault-threat-model-required",
            )
            .with_context(serde_json::json!({ "code": "vault_threat_model_missing" })));
        }

        // Step 3: Verify secret exists before issuing grant (fail fast; no Zeroizing held)
        let _ = self
            .keychain
            .get_secret(&opts.label)
            .map_err(from_keychain_err)?;

        // Step 4: Generate opaque grant ID — ULID, no secret material
        let grant_id = new_grant_id();
        let issued_at_ms = now_ms();

        // Step 5: Store grant record
        {
            let mut grants = self.grants.write();
            grants.insert(
                grant_id.clone(),
                Grant {
                    session_id: session.clone(),
                    label: opts.label.clone(),
                    origin: opts.origin.clone(),
                    scopes: opts.scopes.clone(),
                    issued_at_ms,
                    ttl_ms: opts.ttl_ms,
                    revoked: false,
                },
            );
        }

        // Step 6: Emit GrantIssued audit entry
        let payload = VaultAuditPayload {
            grant_id: &grant_id.0,
            origin: &opts.origin,
            credential_label: &opts.label,
            requested_scopes: &opts.scopes,
            result: "issued",
            triggering_action_id: None,
            ts_tick: issued_at_ms,
        };
        let _ = self.manifest_writer.append_audit(
            session,
            AuditKind::GrantIssued,
            audit_bytes(&payload),
        );

        Ok(grant_id)
    }

    fn substitute(&self, grant: GrantId, req: &mut NetRequest) -> Result<(), LoomError> {
        // Clone grant fields under READ lock — avoids TOCTOU between check and use.
        let (label, grant_origin, grant_scopes, issued_at_ms, ttl_ms, revoked, session_id) = {
            let grants = self.grants.read();
            let g = grants.get(&grant).ok_or_else(|| {
                LoomError::new(LoomErrorCode::VaultRejection, "vault-grant-not-found")
            })?;
            (
                g.label.clone(),
                g.origin.clone(),
                g.scopes.clone(),
                g.issued_at_ms,
                g.ttl_ms,
                g.revoked,
                g.session_id.clone(),
            )
        };

        let now = now_ms();

        // 4-check sequence — revoked → origin → scopes → ttl

        // Check 1: Revoked
        if revoked {
            let payload = VaultAuditPayload {
                grant_id: &grant.0,
                origin: &grant_origin,
                credential_label: &label,
                requested_scopes: &req.scopes,
                result: "denied",
                triggering_action_id: None,
                ts_tick: now,
            };
            let _ = self.manifest_writer.append_audit(
                session_id,
                AuditKind::GrantRejected,
                audit_bytes(&payload),
            );
            return Err(LoomError::new(
                LoomErrorCode::VaultGrantRevoked,
                "grant revoked",
            ));
        }

        // Check 2: Origin match
        if req.origin != grant_origin {
            let payload = VaultAuditPayload {
                grant_id: &grant.0,
                origin: &grant_origin,
                credential_label: &label,
                requested_scopes: &req.scopes,
                result: "denied",
                triggering_action_id: None,
                ts_tick: now,
            };
            let _ = self.manifest_writer.append_audit(
                session_id,
                AuditKind::GrantRejected,
                audit_bytes(&payload),
            );
            return Err(
                LoomError::new(LoomErrorCode::VaultRejection, "vault-origin-mismatch")
                    .with_context(serde_json::json!({
                        "code": "vault_origin_mismatch",
                        "details": {
                            "expected_origin": grant_origin,
                            "observed_origin": req.origin
                        }
                    })),
            );
        }

        // Check 3: Scopes superset — grant.scopes ⊇ req.scopes
        for req_scope in &req.scopes {
            if !grant_scopes.contains(req_scope) {
                let payload = VaultAuditPayload {
                    grant_id: &grant.0,
                    origin: &grant_origin,
                    credential_label: &label,
                    requested_scopes: &req.scopes,
                    result: "denied",
                    triggering_action_id: None,
                    ts_tick: now,
                };
                let _ = self.manifest_writer.append_audit(
                    session_id,
                    AuditKind::GrantRejected,
                    audit_bytes(&payload),
                );
                return Err(LoomError::new(
                    LoomErrorCode::VaultRejection,
                    "vault-scope-insufficient",
                )
                .with_context(serde_json::json!({
                    "code": "vault_scope_insufficient",
                    "details": {
                        "required_scope": req_scope,
                        "granted_scopes": grant_scopes
                    }
                })));
            }
        }

        // Check 4: TTL
        if now > issued_at_ms.saturating_add(ttl_ms) {
            let payload = VaultAuditPayload {
                grant_id: &grant.0,
                origin: &grant_origin,
                credential_label: &label,
                requested_scopes: &req.scopes,
                result: "expired",
                triggering_action_id: None,
                ts_tick: now,
            };
            let _ = self.manifest_writer.append_audit(
                session_id,
                AuditKind::GrantExpired,
                audit_bytes(&payload),
            );
            return Err(
                LoomError::new(LoomErrorCode::VaultGrantExpired, "grant ttl exceeded")
                    .with_context(serde_json::json!({
                        "code": "vault_grant_expired",
                        "details": {
                            "expired_at": issued_at_ms.saturating_add(ttl_ms),
                            "observed_at": now
                        }
                    })),
            );
        }

        // Fetch secret from keychain — Zeroizing<Vec<u8>> zeroizes on drop
        let secret: Zeroizing<Vec<u8>> = self
            .keychain
            .get_secret(&label)
            .map_err(from_keychain_err)?;

        // Write Authorization header in-place — the SINGLE site for raw secret bytes.
        // `secret` zeroizes when dropped at end of this scope.
        req.headers.insert(
            "Authorization".to_string(),
            format!("Bearer {}", String::from_utf8_lossy(&secret)),
        );
        drop(secret); // explicit zeroize

        // Emit GrantConsumed audit (also covers "secret_fetched_from_keychain")
        let payload = VaultAuditPayload {
            grant_id: &grant.0,
            origin: &grant_origin,
            credential_label: &label,
            requested_scopes: &req.scopes,
            result: "consumed",
            triggering_action_id: None,
            ts_tick: now,
        };
        let _ = self.manifest_writer.append_audit(
            session_id,
            AuditKind::GrantConsumed,
            audit_bytes(&payload),
        );

        Ok(())
    }

    fn revoke(&self, grant: GrantId, reason: RevokeReason) -> Result<(), LoomError> {
        let (label, origin, scopes, session_id) = {
            let mut grants = self.grants.write();
            let g = grants.get_mut(&grant).ok_or_else(|| {
                LoomError::new(LoomErrorCode::VaultRejection, "vault-grant-not-found")
            })?;
            let label = g.label.clone();
            let origin = g.origin.clone();
            let scopes = g.scopes.clone();
            let session_id = g.session_id.clone();
            g.revoked = true;
            (label, origin, scopes, session_id)
        };

        let now = now_ms();
        let payload = VaultAuditPayload {
            grant_id: &grant.0,
            origin: &origin,
            credential_label: &label,
            requested_scopes: &scopes,
            result: "revoked",
            triggering_action_id: None,
            ts_tick: now,
        };
        let _ = self.manifest_writer.append_audit(
            session_id,
            AuditKind::GrantRevoked,
            audit_bytes(&payload),
        );
        drop(reason);
        Ok(())
    }

    fn add_credential(&self, opts: AddCredentialOpts) -> Result<AddCredentialReceipt, LoomError> {
        // OAuth-only allowlist. Non-allowlisted providers
        // reject with the canonical `vault_credential_type_unsupported`
        // envelope (`details.allowed_types = ["oauth2_authorization_code_pkce"]`).
        if !OAUTH_PROVIDER_ALLOWLIST.contains(&opts.provider.as_str()) {
            return Err(
                LoomError::new(LoomErrorCode::VaultRejection, "vault-oauth-only").with_context(
                    serde_json::json!({
                        "code": "vault_credential_type_unsupported",
                        "details": { "allowed_types": ["oauth2_authorization_code_pkce"] }
                    }),
                ),
            );
        }

        // Allowlisted but real OAuth device flow not yet implemented — return
        // typed `oauth_required` receipt (Q2 stub branch). No keychain write,
        // no audit append; the real flow lands in a follow-up feature.
        let label = opts
            .label
            .unwrap_or_else(|| format!("{}/oauth_token", opts.provider));
        Ok(AddCredentialReceipt {
            provider: opts.provider,
            label,
            status: "oauth_required".to_string(),
        })
    }

    fn list_grants(&self, session: Option<SessionId>) -> Result<Vec<GrantSnapshot>, LoomError> {
        let grants = self.grants.read();
        let now = now_ms();
        let snapshots = grants
            .iter()
            .filter(|(_, g)| !g.revoked)
            .filter(|(_, g)| now <= g.issued_at_ms.saturating_add(g.ttl_ms)) // F-A7: TTL-aware
            .filter(|(_, g)| session.as_ref().is_none_or(|s| &g.session_id == s))
            .map(|(gid, g)| GrantSnapshot {
                grant_id: gid.0.clone(),
                session_id: g.session_id.0.clone(),
                origin: g.origin.clone(),
                scopes: g.scopes.clone(),
                ttl_seconds: g.ttl_ms / 1000,
                label: g.label.clone(),
            })
            .collect();
        Ok(snapshots)
    }

    // ─── v0.9.4 credential-management methods (W5 full) ───────────
    // Each method:
    //   1. (G5b) Append `SecretOpPending{label, op}` audit if `session.is_some()`.
    //   2. Dispatch to backend via `BlockingKeychain` (spawn_blocking + timeout).
    //   3. (G5a) On success: append the typed `Secret*Stored/Fetched/Deleted/Listed`
    //      audit; on failure: append `Secret*Failed{label, reason, internal_hash}`.
    //   4. Translate `KeychainError → LoomError` per A-W5.2 and bubble up.
    //
    // When `session` is `None` (sessionless `loom vault add`) the audit
    // writes are skipped entirely — only `tracing::info!`/`tracing::error!`
    // give visibility. Per the plan there is no global "ops" manifest;
    // sessionless audit is a documented v0.9.5 follow-up.

    fn set_secret(
        &self,
        session: Option<&SessionId>,
        label: &str,
        secret: Zeroizing<Vec<u8>>,
    ) -> Result<(), LoomError> {
        let byte_count = secret.len();
        tracing::info!(label = %label, byte_count, "vault.set_secret");

        // Pre-existence check feeds the `replaced` field of the success audit
        // and the W5.9/A-W8.6 ownership contract (deferred — see comment
        // at end of method).
        let pre_existed = self.keychain.get_secret(label).is_ok();

        self.append_secret_op_pending(session, label, SecretOp::Set);

        match self.keychain.set_secret(label, secret) {
            Ok(()) => {
                self.append_secret_audit(
                    session,
                    AuditKind::SecretStored,
                    &SecretAuditPayload::Stored {
                        label,
                        size_bucket: size_bucket(byte_count),
                        replaced: pre_existed,
                    },
                );
                if pre_existed {
                    // Distinct audit kind so operators can grep replace events
                    // separately from first-write events (the `replaced` field
                    // inside Stored carries the same signal for JSON consumers).
                    self.append_secret_audit(
                        session,
                        AuditKind::SecretReplaced,
                        &SecretAuditPayload::Stored {
                            label,
                            size_bucket: size_bucket(byte_count),
                            replaced: true,
                        },
                    );
                }
                Ok(())
            }
            Err(err) => {
                self.append_secret_failure(
                    session,
                    AuditKind::SecretStoreFailed,
                    label,
                    &err,
                    SecretFailureSlot::Store,
                );
                Err(from_keychain_err(err))
            }
        }
        // W5.9 / A-W8.6 ownership check on existing label is deferred —
        // depends on W2's `kSecAttrCreator` discriminator (also deferred
        // per the W2 known-limitation comment in loom-keychain/src/macos.rs).
        // Re-enable when the v0.9.5 follow-up wires the lower-level
        // SecItem path that exposes creator inspection.
    }

    fn get_secret_direct(
        &self,
        session: Option<&SessionId>,
        label: &str,
    ) -> Result<Zeroizing<Vec<u8>>, LoomError> {
        tracing::info!(label = %label, "vault.get_secret_direct");
        self.append_secret_op_pending(session, label, SecretOp::Get);

        match self.keychain.get_secret(label) {
            Ok(bytes) => {
                self.append_secret_audit(
                    session,
                    AuditKind::SecretFetched,
                    &SecretAuditPayload::Fetched { label },
                );
                Ok(bytes)
            }
            Err(err) => {
                self.append_secret_failure(
                    session,
                    AuditKind::SecretFetchFailed,
                    label,
                    &err,
                    SecretFailureSlot::Fetch,
                );
                Err(from_keychain_err(err))
            }
        }
    }

    fn delete_secret(
        &self,
        session: Option<&SessionId>,
        label: &str,
        force: bool,
    ) -> Result<DeleteSecretOutcome, LoomError> {
        tracing::info!(label = %label, force, "vault.delete_secret");
        self.append_secret_op_pending(session, label, SecretOp::Delete);

        // D29 cascade semantics: collect alive grants that reference this
        // label, then either error (default) or revoke them all (force).
        let now = now_ms();
        let referencing_grants: Vec<GrantId> = {
            let grants = self.grants.read();
            grants
                .iter()
                .filter(|(_, g)| {
                    g.label == label
                        && !g.revoked
                        && now < g.issued_at_ms.saturating_add(g.ttl_ms)
                })
                .map(|(id, _)| id.clone())
                .collect()
        };

        if !force && !referencing_grants.is_empty() {
            // SecretDeleteFailed audit not emitted — the keychain backend
            // was never called. Use VaultRejection with a structured code
            // so the CLI can render the actionable message + exit code 1.
            return Err(LoomError::new(
                LoomErrorCode::VaultRejection,
                format!(
                    "credential '{label}' is in use by {} active grant(s); pass --force to cascade-revoke",
                    referencing_grants.len()
                ),
            )
            .with_context(serde_json::json!({
                "code": "credential_in_use",
                "label": label,
                "active_grants": referencing_grants.len(),
            })));
        }

        let mut cascade_revoked: u32 = 0;
        if force {
            // Revoke each referencing grant. Mirrors `Vault::revoke` flow
            // (sets `revoked = true`, appends GrantRevoked audit per grant
            // to that grant's session manifest).
            for gid in &referencing_grants {
                if let Err(e) = self.revoke(
                    gid.clone(),
                    RevokeReason {
                        reason: "credential_deleted".to_string(),
                    },
                ) {
                    tracing::warn!(grant_id = %gid.0, error = %e, "cascade revoke failed");
                    // Best-effort — D29 / plan §6 Non-Goals A-W8.6 #15: partial
                    // failure mid-cascade leaves the keychain entry intact.
                    return Err(e);
                }
                cascade_revoked = cascade_revoked.saturating_add(1);
            }
        }

        match self.keychain.delete_secret(label) {
            Ok(()) => {
                self.append_secret_audit(
                    session,
                    AuditKind::SecretDeleted,
                    &SecretAuditPayload::Deleted {
                        label,
                        cascade_revoked_grants: cascade_revoked,
                    },
                );
                Ok(DeleteSecretOutcome {
                    cascade_revoked_grants: cascade_revoked,
                })
            }
            Err(err) => {
                self.append_secret_failure(
                    session,
                    AuditKind::SecretDeleteFailed,
                    label,
                    &err,
                    SecretFailureSlot::Delete,
                );
                Err(from_keychain_err(err))
            }
        }
    }

    fn list_labels(&self, session: Option<&SessionId>) -> Result<Vec<String>, LoomError> {
        tracing::info!("vault.list_labels");
        // D14: NO pre-op `SecretOpPending` for list (it's not "intent to
        // mutate"). One success/failure audit per call.

        match self.keychain.list_labels() {
            Ok(labels) => {
                self.append_secret_audit(
                    session,
                    AuditKind::SecretsListed,
                    &SecretAuditPayload::Listed {
                        count: u32::try_from(labels.len()).unwrap_or(u32::MAX),
                        // service_id is currently hardcoded `"loom"` per D36;
                        // when the follow-up makes it configurable, source
                        // from the keychain config.
                        service_id: "loom",
                    },
                );
                Ok(labels)
            }
            Err(err) => {
                // FetchFailed slot is the closest analogue (list is a
                // read-side op); operators consuming the audit chain
                // distinguish via the AuditKind tag.
                self.append_secret_failure(
                    session,
                    AuditKind::SecretFetchFailed,
                    "<list>",
                    &err,
                    SecretFailureSlot::Fetch,
                );
                Err(from_keychain_err(err))
            }
        }
    }
}

/// Internal discriminator for which success-side audit kind a failure
/// belongs to — used to keep the `append_secret_failure` helper compact
/// without three near-identical method bodies.
#[derive(Debug, Clone, Copy)]
enum SecretFailureSlot {
    Store,
    Delete,
    Fetch,
}

impl LocalVault {
    /// Append a G5b `SecretOpPending` audit for the named op. No-op when
    /// `session` is `None` (sessionless CLI flow). Failures to write the
    /// audit are logged-and-swallowed — same convention as the existing
    /// `grant`/`consume`/`revoke` flows above.
    // All three audit helpers below (`append_secret_op_pending`,
    // `append_secret_audit`, `append_secret_failure`) intentionally swallow
    // `manifest_writer::append_audit` errors after a `tracing::warn!` echo.
    // Per the threat model (G5a post-op + G5b pre-op intent), audit is
    // best-effort observability — NOT a security gate. A full disk or a
    // transiently-broken WAL must not abort the user's vault operation,
    // because the operation either already happened (G5a) or is about to
    // happen anyway (G5b). Operators correlate audit-write failures via
    // the `warn`-level log; the absence of an audit entry is itself
    // observable.
    fn append_secret_op_pending(
        &self,
        session: Option<&SessionId>,
        label: &str,
        op: SecretOp,
    ) {
        let Some(session) = session else { return };
        let payload = SecretAuditPayload::OpPending { label, op };
        let bytes = secret_audit_bytes(&payload);
        if let Err(e) = self.manifest_writer.append_audit(
            session.clone(),
            AuditKind::SecretOpPending,
            bytes,
        ) {
            tracing::warn!(error = %e, "append SecretOpPending failed");
        }
    }

    /// Append a success-side audit (`SecretStored`/`SecretFetched`/
    /// `SecretDeleted`/`SecretsListed`/`SecretReplaced`). No-op when
    /// `session` is `None`.
    fn append_secret_audit(
        &self,
        session: Option<&SessionId>,
        kind: AuditKind,
        payload: &SecretAuditPayload<'_>,
    ) {
        let Some(session) = session else { return };
        let bytes = secret_audit_bytes(payload);
        let kind_for_log = kind.clone();
        if let Err(e) = self.manifest_writer.append_audit(session.clone(), kind, bytes) {
            tracing::warn!(error = %e, kind = ?kind_for_log, "append secret audit failed");
        }
    }

    /// Append a failure-side audit (`Secret*Failed`). The original error
    /// message is hashed into `internal_hash` for support correlation
    /// (A-W6.3) — operators paste the hash into the daemon log to
    /// recover the original message; the message itself never reaches
    /// the persistent manifest.
    fn append_secret_failure(
        &self,
        session: Option<&SessionId>,
        kind: AuditKind,
        label: &str,
        err: &KeychainError,
        slot: SecretFailureSlot,
    ) {
        // A-W6.3: structured tracing::error! echo with the internal_hash
        // for ALL Internal-kind errors so support can correlate.
        if matches!(err.kind(), KeychainErrorKind::Internal) {
            tracing::error!(
                internal_hash = err.internal_hash().unwrap_or("<missing>"),
                original_message = %err.message(),
                "vault.{} internal error",
                match slot {
                    SecretFailureSlot::Store => "set_secret",
                    SecretFailureSlot::Delete => "delete_secret",
                    SecretFailureSlot::Fetch => "get_secret",
                }
            );
        }

        let Some(session) = session else { return };
        let reason = secret_reason(err);
        let internal_hash = err.internal_hash();
        let payload = match slot {
            SecretFailureSlot::Store => SecretAuditPayload::StoreFailed {
                label,
                reason,
                internal_hash,
            },
            SecretFailureSlot::Delete => SecretAuditPayload::DeleteFailed {
                label,
                reason,
                internal_hash,
            },
            SecretFailureSlot::Fetch => SecretAuditPayload::FetchFailed {
                label,
                reason,
                internal_hash,
            },
        };
        let bytes = secret_audit_bytes(&payload);
        let kind_for_log = kind.clone();
        if let Err(e) = self.manifest_writer.append_audit(session.clone(), kind, bytes) {
            tracing::warn!(error = %e, kind = ?kind_for_log, "append secret failure audit failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::LoomErrorCode;
    use crate::manifest_writer::manifest_writer::SessionId;
    use crate::manifest_writer::{LocalManifestWriter, ManifestWriter};
    use crate::observability::Observability;
    use crate::vault::vault::{
        AddCredentialOpts, CredentialType, Grant, GrantId, GrantOpts, KeychainAccess, LocalVault,
        NetRequest, RevokeReason, Vault,
    };
    use loom_keychain::{KeychainError, KeychainErrorKind};
    use serde_json::Value;
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use zeroize::Zeroizing;

    struct StubKeychain {
        label: String,
        secret: Vec<u8>,
    }

    impl KeychainAccess for StubKeychain {
        fn get_secret(&self, label: &str) -> Result<Zeroizing<Vec<u8>>, KeychainError> {
            if label == self.label {
                Ok(Zeroizing::new(self.secret.clone()))
            } else {
                Err(KeychainError::new(
                    KeychainErrorKind::NotFound,
                    "label not found",
                ))
            }
        }

        fn set_secret(
            &self,
            _label: &str,
            _secret: Zeroizing<Vec<u8>>,
        ) -> Result<(), KeychainError> {
            Err(KeychainError::new(
                KeychainErrorKind::Unavailable,
                "vault unit-test stub does not exercise set",
            ))
        }

        fn delete_secret(&self, _label: &str) -> Result<(), KeychainError> {
            Err(KeychainError::new(
                KeychainErrorKind::Unavailable,
                "vault unit-test stub does not exercise delete",
            ))
        }

        fn list_labels(&self) -> Result<Vec<String>, KeychainError> {
            Err(KeychainError::new(
                KeychainErrorKind::Unavailable,
                "vault unit-test stub does not exercise list",
            ))
        }
    }

    const TEST_LABEL: &str = "github.com/oauth_token";
    const TEST_SECRET: &[u8] = b"secret-token-bytes";
    const TEST_ORIGIN: &str = "api.github.com";

    /// Returns (vault, manifest_writer, session_id, sessions_dir).
    /// Each call gets a unique session + dir so parallel tests don't share a WAL.
    fn fixture() -> (LocalVault, Arc<LocalManifestWriter>, SessionId) {
        let unique = ulid::Ulid::new().to_string();
        let sessions_root = std::env::temp_dir().join(format!("loom-vault-test-{unique}"));
        std::fs::create_dir_all(&sessions_root).ok();

        let obs = Observability::new(sessions_root.join("test.log"), false);
        let mw = Arc::new(LocalManifestWriter::new(sessions_root, obs.clone()));
        let sid = SessionId(unique);
        mw.open_manifest(sid.clone(), None).ok();

        let kc: Arc<dyn KeychainAccess> = Arc::new(StubKeychain {
            label: TEST_LABEL.to_string(),
            secret: TEST_SECRET.to_vec(),
        });
        let vault = LocalVault::new(kc, mw.clone() as Arc<dyn ManifestWriter>, obs);
        (vault, mw, sid)
    }

    fn default_opts() -> GrantOpts {
        GrantOpts {
            credential_type: CredentialType::OAuth,
            label: TEST_LABEL.to_string(),
            origin: TEST_ORIGIN.to_string(),
            scopes: vec!["repo:read".to_string()],
            ttl_ms: 600_000,
            threat_model_acknowledged: true,
        }
    }

    fn net_req(origin: &str, scopes: &[&str]) -> NetRequest {
        NetRequest {
            url: format!("https://{origin}/api"),
            method: "GET".to_string(),
            headers: BTreeMap::new(),
            body: vec![],
            origin: origin.to_string(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn read_audit_entries(mw: &LocalManifestWriter, sid: &SessionId) -> Vec<Value> {
        let wal = mw.sessions_root.join(&sid.0).join("manifest.wal");
        let content = std::fs::read_to_string(wal).unwrap_or_default();
        content
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["kind"] == "audit_entry")
            .collect()
    }

    fn audit_payload_from_entry(entry: &Value) -> Option<Value> {
        let bytes_arr = entry["canonical_bytes"].as_array()?;
        let bytes: Vec<u8> = bytes_arr
            .iter()
            .filter_map(|v| v.as_u64().map(|n| n as u8))
            .collect();
        serde_json::from_slice(&bytes).ok()
    }

    // ── Threat model document prerequisite ──────────────────

    #[test]
    fn vault_threat_model_prerequisite() {
        // CARGO_MANIFEST_DIR = <workspace>/loom-core
        // ../../ = <workspace>
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let threat_model = manifest_dir.join("../security/vault_threat_model.md");
        assert!(
            threat_model.exists(),
            "vault_threat_model.md must exist; checked at {}",
            threat_model.display()
        );
        let content =
            std::fs::read_to_string(&threat_model).expect("vault_threat_model.md must be readable");
        assert!(
            content.starts_with("# Vault Threat Model"),
            "first line must be '# Vault Threat Model'"
        );
        for section in &[
            "## Attacker Classes",
            "## Security Goals",
            "## Trust Boundaries",
            "## Abuse Cases",
        ] {
            assert!(
                content.contains(section),
                "vault_threat_model.md missing required section: {section}"
            );
        }
    }

    // ── threat_model_acknowledged gate ─────────────

    #[test]
    fn grant_requires_threat_model_acknowledged() {
        let (vault, _mw, sid) = fixture();
        let mut opts = default_opts();
        opts.threat_model_acknowledged = false;
        let err = vault.grant(sid.clone(), opts).unwrap_err();
        assert_eq!(err.code, LoomErrorCode::VaultRejection);
        let ctx = err.context.unwrap();
        assert_eq!(ctx["code"], "vault_threat_model_missing");
    }

    // ── OAuth-only enforcement ─────────────────────────────

    #[test]
    fn grant_rejects_api_key_type() {
        let (vault, _mw, sid) = fixture();
        let mut opts = default_opts();
        opts.credential_type = CredentialType::ApiKey;
        let err = vault.grant(sid.clone(), opts).unwrap_err();
        assert_eq!(err.code, LoomErrorCode::VaultRejection);
        let ctx = err.context.unwrap();
        assert_eq!(ctx["code"], "vault_credential_type_unsupported");
        assert!(ctx["details"]["allowed_types"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "oauth2_authorization_code_pkce"));
    }

    #[test]
    fn grant_rejects_saml_type() {
        let (vault, _mw, sid) = fixture();
        let mut opts = default_opts();
        opts.credential_type = CredentialType::Saml;
        let err = vault.grant(sid.clone(), opts).unwrap_err();
        assert_eq!(err.code, LoomErrorCode::VaultRejection);
    }

    #[test]
    fn grant_rejects_basic_type() {
        let (vault, _mw, sid) = fixture();
        let mut opts = default_opts();
        opts.credential_type = CredentialType::Basic;
        let err = vault.grant(sid.clone(), opts).unwrap_err();
        assert_eq!(err.code, LoomErrorCode::VaultRejection);
    }

    // ── Grant issuance — no secret in GrantId ──────────────

    #[test]
    fn grant_returns_grant_id_not_containing_secret() {
        let (vault, _mw, sid) = fixture();
        let gid = vault.grant(sid.clone(), default_opts()).unwrap();
        let secret_str = std::str::from_utf8(TEST_SECRET).unwrap();
        // No 6+ char substring of the secret may appear in the GrantId
        for start in 0..secret_str.len().saturating_sub(5) {
            let end = (start + 6).min(secret_str.len());
            let sub = &secret_str[start..end];
            assert!(
                !gid.0.contains(sub),
                "GrantId contains secret substring '{sub}'"
            );
        }
    }

    #[test]
    fn grant_ids_are_unique() {
        let (vault, _mw, sid) = fixture();
        let g1 = vault.grant(sid.clone(), default_opts()).unwrap();
        let g2 = vault.grant(sid.clone(), default_opts()).unwrap();
        assert_ne!(g1.0, g2.0);
    }

    // ── Origin mismatch rejection ──────────────────────────

    #[test]
    fn substitute_rejects_origin_mismatch() {
        let (vault, _mw, sid) = fixture();
        let gid = vault.grant(sid.clone(), default_opts()).unwrap();
        let mut r = net_req("api.gitlab.com", &["repo:read"]);
        let err = vault.substitute(gid, &mut r).unwrap_err();
        assert_eq!(err.code, LoomErrorCode::VaultRejection);
        let ctx = err.context.unwrap();
        assert_eq!(ctx["code"], "vault_origin_mismatch");
        assert_eq!(ctx["details"]["expected_origin"], TEST_ORIGIN);
        assert_eq!(ctx["details"]["observed_origin"], "api.gitlab.com");
        assert!(
            !r.headers.contains_key("Authorization"),
            "no header on rejection"
        );
    }

    // ── Scope escalation rejection ─────────────────────────

    #[test]
    fn substitute_rejects_scope_escalation() {
        let (vault, _mw, sid) = fixture();
        let gid = vault.grant(sid.clone(), default_opts()).unwrap();
        let mut r = net_req(TEST_ORIGIN, &["repo:read", "repo:write"]);
        let err = vault.substitute(gid, &mut r).unwrap_err();
        assert_eq!(err.code, LoomErrorCode::VaultRejection);
        let ctx = err.context.unwrap();
        assert_eq!(ctx["code"], "vault_scope_insufficient");
        assert_eq!(ctx["details"]["required_scope"], "repo:write");
        assert!(!r.headers.contains_key("Authorization"));
    }

    // ── TTL expiry rejection ───────────────────────────────

    #[test]
    fn substitute_rejects_expired_grant() {
        let (vault, _mw, sid) = fixture();
        // Insert an already-expired grant (issued_at_ms=0, ttl_ms=1)
        let gid = GrantId("EXPIRED00000000000000000000".to_string());
        {
            let mut grants = vault.grants.write();
            grants.insert(
                gid.clone(),
                Grant {
                    session_id: sid.clone(),
                    label: TEST_LABEL.to_string(),
                    origin: TEST_ORIGIN.to_string(),
                    scopes: vec!["repo:read".to_string()],
                    issued_at_ms: 0,
                    ttl_ms: 1,
                    revoked: false,
                },
            );
        }
        let mut r = net_req(TEST_ORIGIN, &["repo:read"]);
        let err = vault.substitute(gid, &mut r).unwrap_err();
        assert_eq!(err.code, LoomErrorCode::VaultGrantExpired);
        let ctx = err.context.unwrap();
        assert_eq!(ctx["code"], "vault_grant_expired");
        assert!(ctx["details"]["expired_at"].as_u64().unwrap() == 1);
        assert!(ctx["details"]["observed_at"].as_u64().unwrap() > 1);
        assert!(!r.headers.contains_key("Authorization"));
    }

    // ── substitute() success path ─────────────────────────────────────────

    #[test]
    fn substitute_writes_authorization_header() {
        let (vault, _mw, sid) = fixture();
        let gid = vault.grant(sid.clone(), default_opts()).unwrap();
        let mut r = net_req(TEST_ORIGIN, &["repo:read"]);
        assert!(!r.headers.contains_key("Authorization"));
        vault.substitute(gid, &mut r).unwrap();
        let auth = r
            .headers
            .get("Authorization")
            .expect("Authorization must be set");
        assert!(auth.starts_with("Bearer "), "must be a Bearer token");
    }

    #[test]
    fn substitute_returns_unit_not_secret() {
        let (vault, _mw, sid) = fixture();
        let gid = vault.grant(sid.clone(), default_opts()).unwrap();
        let mut r = net_req(TEST_ORIGIN, &["repo:read"]);
        // Return type is () — no secret in return value
        let result: Result<(), LoomError> = vault.substitute(gid, &mut r);
        assert!(result.is_ok());
    }

    // ── Revoke lifecycle ──────────────────────────────────────────────────

    #[test]
    fn substitute_rejects_revoked_grant() {
        let (vault, _mw, sid) = fixture();
        let gid = vault.grant(sid.clone(), default_opts()).unwrap();
        vault
            .revoke(
                gid.clone(),
                RevokeReason {
                    reason: "user_request".to_string(),
                },
            )
            .unwrap();
        let mut r = net_req(TEST_ORIGIN, &["repo:read"]);
        let err = vault.substitute(gid, &mut r).unwrap_err();
        assert_eq!(err.code, LoomErrorCode::VaultGrantRevoked);
        assert!(!r.headers.contains_key("Authorization"));
    }

    // ── Audit entries in order ────────────────────────────

    #[test]
    fn audit_entries_in_order_issued_consumed_revoked() {
        let (vault, mw, sid) = fixture();
        let gid = vault.grant(sid.clone(), default_opts()).unwrap();
        let mut r = net_req(TEST_ORIGIN, &["repo:read"]);
        vault.substitute(gid.clone(), &mut r).unwrap();
        vault
            .revoke(
                gid,
                RevokeReason {
                    reason: "test".to_string(),
                },
            )
            .unwrap();

        let entries = read_audit_entries(&mw, &sid);
        assert!(
            entries.len() >= 3,
            "expected ≥3 audit entries, got {}",
            entries.len()
        );

        let kinds: Vec<&str> = entries
            .iter()
            .filter_map(|e| e["audit_kind"].as_str())
            .collect();

        let issued_pos = kinds
            .iter()
            .position(|&k| k == "grant_issued")
            .expect("missing grant_issued audit entry");
        let consumed_pos = kinds
            .iter()
            .position(|&k| k == "grant_consumed")
            .expect("missing grant_consumed audit entry");
        let revoked_pos = kinds
            .iter()
            .position(|&k| k == "grant_revoked")
            .expect("missing grant_revoked audit entry");

        assert!(
            issued_pos < consumed_pos,
            "grant_issued must precede grant_consumed"
        );
        assert!(
            consumed_pos < revoked_pos,
            "grant_consumed must precede grant_revoked"
        );
    }

    // ── Audit payload completeness ───────────────────────

    #[test]
    fn audit_payload_has_all_required_fields() {
        let (vault, mw, sid) = fixture();
        let gid = vault.grant(sid.clone(), default_opts()).unwrap();
        let mut r = net_req(TEST_ORIGIN, &["repo:read"]);
        vault.substitute(gid, &mut r).unwrap();

        let entries = read_audit_entries(&mw, &sid);
        assert!(!entries.is_empty(), "must have audit entries");

        for entry in &entries {
            let Some(payload) = audit_payload_from_entry(entry) else {
                continue;
            };
            for field in &[
                "grant_id",
                "origin",
                "credential_label",
                "requested_scopes",
                "result",
                "ts_tick",
            ] {
                assert!(
                    !payload[field].is_null(),
                    "audit payload missing required field '{field}'"
                );
            }
        }
    }

    // ── No secret in grant response ─────────────────────

    #[test]
    fn no_secret_in_grant_id_response() {
        let (vault, _mw, sid) = fixture();
        let gid = vault.grant(sid.clone(), default_opts()).unwrap();
        let secret_str = std::str::from_utf8(TEST_SECRET).unwrap();
        assert!(
            !gid.0.contains(secret_str),
            "GrantId must not contain secret"
        );
    }

    #[test]
    fn no_secret_in_audit_entries() {
        let (vault, mw, sid) = fixture();
        let gid = vault.grant(sid.clone(), default_opts()).unwrap();
        let mut r = net_req(TEST_ORIGIN, &["repo:read"]);
        vault.substitute(gid, &mut r).unwrap();

        let entries = read_audit_entries(&mw, &sid);
        let secret_str = std::str::from_utf8(TEST_SECRET).unwrap();
        for entry in &entries {
            let entry_str = entry.to_string();
            for start in 0..secret_str.len().saturating_sub(5) {
                let end = (start + 6).min(secret_str.len());
                let sub = &secret_str[start..end];
                assert!(
                    !entry_str.contains(sub),
                    "audit entry contains secret substring '{sub}'"
                );
            }
        }
    }

    // ── No plaintext store.bin ────────────────────────────

    #[test]
    fn no_plaintext_vault_store_file() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let store_bin = std::path::Path::new(&home).join(".loom/vault/store.bin");
        if store_bin.exists() {
            let bytes = std::fs::read(&store_bin).unwrap_or_default();
            let content = std::str::from_utf8(&bytes).unwrap_or("");
            let secret_str = std::str::from_utf8(TEST_SECRET).unwrap();
            for start in 0..secret_str.len().saturating_sub(5) {
                let end = (start + 6).min(secret_str.len());
                let sub = &secret_str[start..end];
                assert!(
                    !content.contains(sub),
                    "store.bin contains plaintext token substring '{sub}'"
                );
            }
        }
        // If store.bin doesn't exist, this confirms keychain-only storage.
    }

    // ── Grants are reusable until revoked/expired ─────────────────────────

    // ── add_credential allowlist + receipt ───────────────

    #[test]
    fn add_credential_rejects_non_allowlisted_provider() {
        let (vault, _mw, _sid) = fixture();
        let err = vault
            .add_credential(AddCredentialOpts {
                provider: "gitlab".to_string(),
                label: None,
                yes: true,
            })
            .unwrap_err();
        assert_eq!(err.code, LoomErrorCode::VaultRejection);
    }

    #[test]
    fn add_credential_returns_typed_receipt_for_github() {
        let (vault, _mw, _sid) = fixture();
        let receipt = vault
            .add_credential(AddCredentialOpts {
                provider: "github".to_string(),
                label: None,
                yes: true,
            })
            .unwrap();
        assert_eq!(receipt.provider, "github");
        assert_eq!(receipt.status, "oauth_required");
        // Default label is `{provider}/oauth_token` when caller omits it.
        assert_eq!(receipt.label, "github/oauth_token");
    }

    #[test]
    fn add_credential_rejects_with_oauth_only_details() {
        // Envelope shape: code = vault_credential_type_unsupported,
        // details.allowed_types = ["oauth2_authorization_code_pkce"].
        let (vault, _mw, _sid) = fixture();
        let err = vault
            .add_credential(AddCredentialOpts {
                provider: "gitlab".to_string(),
                label: None,
                yes: true,
            })
            .unwrap_err();
        let ctx = err.context.unwrap();
        assert_eq!(ctx["code"], "vault_credential_type_unsupported");
        assert!(ctx["details"]["allowed_types"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "oauth2_authorization_code_pkce"));
    }

    // ── list_grants ──────────────────────────────────────

    #[test]
    fn list_grants_returns_empty_when_none() {
        let (vault, _mw, _sid) = fixture();
        let grants = vault.list_grants(None).unwrap();
        assert!(grants.is_empty(), "no grants → empty list");
    }

    #[test]
    fn list_grants_returns_alive_grants() {
        let (vault, _mw, sid) = fixture();
        let _g1 = vault.grant(sid.clone(), default_opts()).unwrap();
        let _g2 = vault.grant(sid.clone(), default_opts()).unwrap();
        let grants = vault.list_grants(None).unwrap();
        assert_eq!(grants.len(), 2);
    }

    #[test]
    fn list_grants_excludes_revoked() {
        let (vault, _mw, sid) = fixture();
        let g1 = vault.grant(sid.clone(), default_opts()).unwrap();
        let _g2 = vault.grant(sid.clone(), default_opts()).unwrap();
        vault
            .revoke(
                g1,
                RevokeReason {
                    reason: "test".to_string(),
                },
            )
            .unwrap();
        let grants = vault.list_grants(None).unwrap();
        assert_eq!(grants.len(), 1, "revoked grant must be excluded");
    }

    #[test]
    fn list_grants_filters_by_session() {
        let (vault, _mw, sid_a) = fixture();
        let sid_b = SessionId(ulid::Ulid::new().to_string());
        let _ga = vault.grant(sid_a.clone(), default_opts()).unwrap();
        let _gb = vault.grant(sid_b.clone(), default_opts()).unwrap();

        let only_a = vault.list_grants(Some(sid_a.clone())).unwrap();
        assert_eq!(only_a.len(), 1);
        assert_eq!(only_a[0].session_id, sid_a.0);

        let only_b = vault.list_grants(Some(sid_b.clone())).unwrap();
        assert_eq!(only_b.len(), 1);
        assert_eq!(only_b[0].session_id, sid_b.0);
    }

    #[test]
    fn list_grants_no_session_filter_returns_all() {
        let (vault, _mw, sid_a) = fixture();
        let sid_b = SessionId(ulid::Ulid::new().to_string());
        let _ga = vault.grant(sid_a.clone(), default_opts()).unwrap();
        let _gb = vault.grant(sid_b.clone(), default_opts()).unwrap();

        let all = vault.list_grants(None).unwrap();
        assert_eq!(all.len(), 2, "None filter → all sessions' grants");
    }

    #[test]
    fn list_grants_includes_grant_id_origin_scopes_ttl() {
        let (vault, _mw, sid) = fixture();
        let opts = default_opts();
        let expected_origin = opts.origin.clone();
        let expected_scopes = opts.scopes.clone();
        let expected_ttl_seconds = opts.ttl_ms / 1000;
        let expected_label = opts.label.clone();
        let gid = vault.grant(sid.clone(), opts).unwrap();

        let grants = vault.list_grants(None).unwrap();
        assert_eq!(grants.len(), 1);
        let snap = &grants[0];
        assert_eq!(snap.grant_id, gid.0);
        assert_eq!(snap.origin, expected_origin);
        assert_eq!(snap.scopes, expected_scopes);
        assert_eq!(snap.ttl_seconds, expected_ttl_seconds);
        assert_eq!(snap.label, expected_label);
    }

    #[test]
    fn list_grants_excludes_expired() {
        // F-A7: list_grants is TTL-aware. An expired but not-revoked grant
        // must NOT appear in the list.
        let (vault, _mw, sid) = fixture();
        let gid = GrantId("EXPIREDLISTABCDEFGHJKMN0000".to_string());
        {
            let mut grants = vault.grants.write();
            grants.insert(
                gid.clone(),
                Grant {
                    session_id: sid.clone(),
                    label: TEST_LABEL.to_string(),
                    origin: TEST_ORIGIN.to_string(),
                    scopes: vec!["repo:read".to_string()],
                    issued_at_ms: 0,
                    ttl_ms: 1,
                    revoked: false,
                },
            );
        }
        let grants = vault.list_grants(None).unwrap();
        assert!(grants.is_empty(), "expired grant must be excluded");
    }

    #[test]
    fn grant_is_reusable_until_revoked() {
        let (vault, _mw, sid) = fixture();
        let gid = vault.grant(sid.clone(), default_opts()).unwrap();
        let mut r1 = net_req(TEST_ORIGIN, &["repo:read"]);
        let mut r2 = net_req(TEST_ORIGIN, &["repo:read"]);
        vault.substitute(gid.clone(), &mut r1).unwrap();
        vault.substitute(gid, &mut r2).unwrap();
        assert!(r1.headers.contains_key("Authorization"));
        assert!(r2.headers.contains_key("Authorization"));
    }

    // ── W5.12 credential-management methods (set/get/delete/list) ───────

    /// Fixture variant backed by `InMemoryKeychain` (mutable, supports
    /// set/get/delete/list) — distinct from the read-only `StubKeychain`
    /// fixture used by Grant lifecycle tests.
    fn fixture_mem() -> (LocalVault, Arc<LocalManifestWriter>, SessionId) {
        use loom_keychain::InMemoryKeychain;
        let unique = ulid::Ulid::new().to_string();
        let sessions_root = std::env::temp_dir().join(format!("loom-vault-mem-{unique}"));
        std::fs::create_dir_all(&sessions_root).ok();

        let obs = Observability::new(sessions_root.join("test.log"), false);
        let mw = Arc::new(LocalManifestWriter::new(sessions_root, obs.clone()));
        let sid = SessionId(unique);
        mw.open_manifest(sid.clone(), None).ok();

        let kc: Arc<dyn KeychainAccess> = Arc::new(InMemoryKeychain::new());
        let vault = LocalVault::new(kc, mw.clone() as Arc<dyn ManifestWriter>, obs);
        (vault, mw, sid)
    }

    #[test]
    fn set_get_round_trip_against_in_memory_backend() {
        let (vault, _mw, sid) = fixture_mem();
        let secret = Zeroizing::new(b"round-trip-bytes".to_vec());
        vault
            .set_secret(Some(&sid), "my-token", secret.clone())
            .expect("set ok");
        let fetched = vault
            .get_secret_direct(Some(&sid), "my-token")
            .expect("get ok");
        assert_eq!(&fetched[..], &secret[..]);
    }

    #[test]
    fn set_secret_silently_upserts() {
        let (vault, _mw, sid) = fixture_mem();
        vault
            .set_secret(Some(&sid), "rotate", Zeroizing::new(b"v1".to_vec()))
            .unwrap();
        vault
            .set_secret(Some(&sid), "rotate", Zeroizing::new(b"v2".to_vec()))
            .unwrap();
        let fetched = vault.get_secret_direct(Some(&sid), "rotate").unwrap();
        assert_eq!(&fetched[..], b"v2");
    }

    #[test]
    fn delete_secret_no_grants_no_force_succeeds() {
        let (vault, _mw, sid) = fixture_mem();
        vault
            .set_secret(Some(&sid), "ephemeral", Zeroizing::new(b"x".to_vec()))
            .unwrap();
        let outcome = vault
            .delete_secret(Some(&sid), "ephemeral", false)
            .expect("delete ok");
        assert_eq!(outcome.cascade_revoked_grants, 0);
        // Subsequent get returns NotFound → VaultUnknownLabel.
        let err = vault
            .get_secret_direct(Some(&sid), "ephemeral")
            .expect_err("get must fail post-delete");
        assert_eq!(err.code, LoomErrorCode::VaultUnknownLabel);
    }

    #[test]
    fn delete_secret_unknown_label_is_idempotent() {
        let (vault, _mw, sid) = fixture_mem();
        let outcome = vault
            .delete_secret(Some(&sid), "never-existed", false)
            .expect("delete-of-missing must be idempotent");
        assert_eq!(outcome.cascade_revoked_grants, 0);
    }

    #[test]
    fn delete_secret_with_active_grants_requires_force_d29() {
        let (vault, _mw, sid) = fixture_mem();
        vault
            .set_secret(Some(&sid), "with-grants", Zeroizing::new(b"x".to_vec()))
            .unwrap();
        // Manually insert a grant referencing this label (skip the
        // full `grant()` flow — we just need an alive Grant record).
        {
            let mut grants = vault.grants.write();
            grants.insert(
                GrantId("01HZACTIVEGRANTAAAAAAAAAAAA".to_string()),
                Grant {
                    session_id: sid.clone(),
                    label: "with-grants".to_string(),
                    origin: "api.example.com".to_string(),
                    scopes: vec!["read".to_string()],
                    issued_at_ms: now_ms(),
                    ttl_ms: 600_000,
                    revoked: false,
                },
            );
        }

        // Default delete fails with VaultRejection { code: credential_in_use }.
        let err = vault
            .delete_secret(Some(&sid), "with-grants", false)
            .expect_err("delete must fail on active grant");
        assert_eq!(err.code, LoomErrorCode::VaultRejection);
        let ctx = err.context.expect("error context");
        assert_eq!(ctx["code"], "credential_in_use");
        assert_eq!(ctx["active_grants"], 1);

        // With --force, the cascade revokes the grant and deletes.
        let outcome = vault
            .delete_secret(Some(&sid), "with-grants", true)
            .expect("force delete ok");
        assert_eq!(outcome.cascade_revoked_grants, 1);
        // Grant is now revoked.
        let grants = vault.grants.read();
        let g = grants
            .get(&GrantId("01HZACTIVEGRANTAAAAAAAAAAAA".to_string()))
            .expect("grant still in map");
        assert!(g.revoked, "grant must be marked revoked");
    }

    #[test]
    fn list_labels_returns_inserted_labels() {
        let (vault, _mw, sid) = fixture_mem();
        vault
            .set_secret(Some(&sid), "alpha", Zeroizing::new(b"1".to_vec()))
            .unwrap();
        vault
            .set_secret(Some(&sid), "beta", Zeroizing::new(b"2".to_vec()))
            .unwrap();
        let mut labels = vault.list_labels(Some(&sid)).expect("list ok");
        labels.sort();
        assert_eq!(labels, vec!["alpha", "beta"]);
    }

    #[test]
    fn sessionless_methods_succeed_without_audit() {
        let (vault, mw, sid) = fixture_mem();
        // Operations work the same with session=None — only the audit
        // chain is skipped. Snapshot the WAL before + after; audit-entry
        // count must be unchanged.
        let before = read_audit_entries(&mw, &sid).len();

        vault
            .set_secret(None, "noaudit", Zeroizing::new(b"x".to_vec()))
            .unwrap();
        let _ = vault.get_secret_direct(None, "noaudit").unwrap();
        vault.delete_secret(None, "noaudit", false).unwrap();
        let _ = vault.list_labels(None).unwrap();

        let after = read_audit_entries(&mw, &sid).len();
        assert_eq!(before, after, "sessionless ops must not append audits");
    }

    #[test]
    fn set_secret_appends_secret_op_pending_then_secret_stored() {
        let (vault, mw, sid) = fixture_mem();
        vault
            .set_secret(Some(&sid), "newentry", Zeroizing::new(b"x".to_vec()))
            .unwrap();
        let entries = read_audit_entries(&mw, &sid);
        let kinds: Vec<String> = entries
            .iter()
            .map(|e| e["audit_kind"].as_str().unwrap_or("").to_string())
            .collect();
        assert!(
            kinds.contains(&"secret_op_pending".to_string()),
            "missing G5b SecretOpPending audit; got {kinds:?}"
        );
        assert!(
            kinds.contains(&"secret_stored".to_string()),
            "missing G5a SecretStored audit; got {kinds:?}"
        );

        let stored = entries
            .iter()
            .find(|e| e["audit_kind"] == "secret_stored")
            .unwrap();
        let payload = audit_payload_from_entry(stored).expect("payload parses");
        assert_eq!(payload["label"], "newentry");
        assert_eq!(payload["size_bucket"], "small");
        assert_eq!(payload["replaced"], false);
    }

    #[test]
    fn set_secret_on_existing_label_emits_secret_replaced() {
        let (vault, mw, sid) = fixture_mem();
        vault
            .set_secret(Some(&sid), "rotate2", Zeroizing::new(b"v1".to_vec()))
            .unwrap();
        vault
            .set_secret(Some(&sid), "rotate2", Zeroizing::new(b"v2".to_vec()))
            .unwrap();
        let entries = read_audit_entries(&mw, &sid);
        let kinds: Vec<String> = entries
            .iter()
            .map(|e| e["audit_kind"].as_str().unwrap_or("").to_string())
            .collect();
        let stored_count = kinds.iter().filter(|k| k.as_str() == "secret_stored").count();
        let replaced_count = kinds.iter().filter(|k| k.as_str() == "secret_replaced").count();
        assert_eq!(stored_count, 2, "two stores → two secret_stored audits");
        assert_eq!(replaced_count, 1, "second store → one secret_replaced audit");
    }

    #[test]
    fn get_missing_label_appends_fetch_failed_with_typed_reason() {
        let (vault, mw, sid) = fixture_mem();
        let err = vault
            .get_secret_direct(Some(&sid), "nope")
            .expect_err("must fail");
        assert_eq!(err.code, LoomErrorCode::VaultUnknownLabel);

        let entries = read_audit_entries(&mw, &sid);
        let failed = entries
            .iter()
            .find(|e| e["audit_kind"] == "secret_fetch_failed")
            .expect("FetchFailed audit");
        let payload = audit_payload_from_entry(failed).expect("payload parses");
        assert_eq!(payload["label"], "nope");
        assert_eq!(payload["reason"], "not_found");
    }

    #[test]
    fn list_labels_appends_secrets_listed_with_count_and_service_id() {
        let (vault, mw, sid) = fixture_mem();
        vault
            .set_secret(Some(&sid), "a", Zeroizing::new(b"1".to_vec()))
            .unwrap();
        vault
            .set_secret(Some(&sid), "b", Zeroizing::new(b"2".to_vec()))
            .unwrap();
        let _ = vault.list_labels(Some(&sid)).unwrap();

        let entries = read_audit_entries(&mw, &sid);
        let listed = entries
            .iter()
            .find(|e| e["audit_kind"] == "secrets_listed")
            .expect("SecretsListed audit");
        let payload = audit_payload_from_entry(listed).expect("payload parses");
        assert_eq!(payload["count"], 2);
        assert_eq!(payload["service_id"], "loom");
    }
}
