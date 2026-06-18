// `impl Vault for LocalVault` — the trait surface (grant / substitute /
// substitute_cookies / revoke / begin_session / end_session / add_credential /
// list_grants + the v0.9.4 credential-management methods).
//
// Split out of the original `impl_local.rs` (large-file reorganization). The
// free-function + `LocalVault` helper internals it leans on live in the sibling
// `helpers` module; the audit-payload DTOs live in `vault/audit_payloads.rs`.

use crate::error::{LoomError, LoomErrorCode};
use crate::manifest_writer::manifest_writer::{AuditKind, SessionId};
use crate::vault::vault::{
    size_bucket, AddCredentialOpts, AddCredentialReceipt, CredentialType, DeleteSecretOutcome,
    Grant, GrantId, GrantOpts, GrantSnapshot, LocalVault, NetRequest, RevokeReason, Vault,
    VaultSessionCtx, OAUTH_PROVIDER_ALLOWLIST,
};
use loom_keychain::KeychainAccess;
use loom_shared::Redacted;
use zeroize::Zeroizing;

use super::helpers::{from_keychain_err, now_ms, vault_session_rng, SecretFailureSlot};

// Vault audit payload DTOs + JCS serialization moved to `audit_payloads.rs`
// (large-file split). Re-imported here; constructed only by the impl below.
use crate::vault::audit_payloads::{
    audit_bytes, extract_cookie_names, SecretAuditPayload, SecretOp, VaultAuditPayload,
};

impl Vault for LocalVault {
    fn grant(&self, session: SessionId, opts: GrantOpts) -> Result<GrantId, LoomError> {
        // Step 1: Per-CredentialType policy (D3, v0.9.5).
        // OAuth + Cookie are allowed; ApiKey/Saml/Basic remain reserved slots
        // that reject with VaultCredentialTypeUnsupported until a future
        // release adds gating logic for them.
        match opts.credential_type {
            CredentialType::OAuth | CredentialType::Cookie => {}
            CredentialType::ApiKey | CredentialType::Saml | CredentialType::Basic => {
                return Err(LoomError::new(
                    LoomErrorCode::VaultRejection,
                    "vault-credential-type-unsupported",
                )
                .with_context(serde_json::json!({
                    "code": "vault_credential_type_unsupported",
                    "details": {
                        "allowed_types": [
                            "oauth2_authorization_code_pkce",
                            "cookie"
                        ]
                    }
                })));
            }
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

        // Step 4: Mint the opaque grant ID — ULID-shaped, no secret
        // material, drawn from the session's seeded audit RNG so the id is
        // identical across two independent same-seed runs (NFR-DET-01).
        // `issued_at_ms` stays wall clock: TTL enforcement only, never in
        // chain-hashed audit bytes.
        let grant_id = self.next_grant_id(&session);
        let issued_at_ms = now_ms();

        // Step 5: Store grant record — keyed by (session, grant_id) since
        // deterministic ids collide across same-seed sessions.
        {
            let mut grants = self.grants.write();
            grants.insert(
                (session.clone(), grant_id.clone()),
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
            ts_tick: self.next_ts_tick(&session),
        };
        let _ = self.manifest_writer.append_audit(
            session,
            AuditKind::GrantIssued,
            audit_bytes(&payload),
        );

        Ok(grant_id)
    }

    fn substitute(
        &self,
        session: &SessionId,
        grant: GrantId,
        req: &mut NetRequest,
    ) -> Result<(), LoomError> {
        // Clone grant fields under READ lock — avoids TOCTOU between check
        // and use. Lookup is session-scoped: a grant issued to another
        // session (or a predicted deterministic id) is not found here.
        let (label, grant_origin, grant_scopes, issued_at_ms, ttl_ms, revoked) = {
            let grants = self.grants.read();
            let g = grants
                .get(&(session.clone(), grant.clone()))
                .ok_or_else(|| {
                    LoomError::new(LoomErrorCode::VaultRejection, "vault-grant-not-found")
                })?;
            (
                g.label.clone(),
                g.origin.clone(),
                g.scopes.clone(),
                g.issued_at_ms,
                g.ttl_ms,
                g.revoked,
            )
        };

        // Wall clock — TTL enforcement only; audit ts_ticks below are the
        // session-relative deterministic vault clock (NFR-DET-01).
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
                ts_tick: self.next_ts_tick(session),
            };
            let _ = self.manifest_writer.append_audit(
                session.clone(),
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
                ts_tick: self.next_ts_tick(session),
            };
            let _ = self.manifest_writer.append_audit(
                session.clone(),
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
                    ts_tick: self.next_ts_tick(session),
                };
                let _ = self.manifest_writer.append_audit(
                    session.clone(),
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
                ts_tick: self.next_ts_tick(session),
            };
            let _ = self.manifest_writer.append_audit(
                session.clone(),
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

        // Build the Authorization value inside zeroize-on-drop storage and
        // park it in the dedicated redacted slot — the SINGLE site for raw
        // secret bytes (G1). Never the plain `headers` map: that map is
        // Debug/Serialize-able and survives the HTTP round-trip un-wiped,
        // which is exactly the TB4 log/serialization leak.
        let mut value = Zeroizing::new(String::with_capacity(7 + secret.len()));
        value.push_str("Bearer ");
        match std::str::from_utf8(&secret) {
            Ok(s) => value.push_str(s),
            Err(_) => {
                // Non-UTF-8 secret: keep the historical lossy encoding on
                // the wire, wiping the intermediate copy.
                let mut lossy = String::from_utf8_lossy(&secret).into_owned();
                value.push_str(&lossy);
                loom_shared::wipe_string_buffer_in_place(&mut lossy);
            }
        }
        req.authorization = Some(Redacted::new(value));
        drop(secret); // explicit zeroize

        // Emit GrantConsumed audit (also covers "secret_fetched_from_keychain")
        let payload = VaultAuditPayload {
            grant_id: &grant.0,
            origin: &grant_origin,
            credential_label: &label,
            requested_scopes: &req.scopes,
            result: "consumed",
            triggering_action_id: None,
            ts_tick: self.next_ts_tick(session),
        };
        let _ = self.manifest_writer.append_audit(
            session.clone(),
            AuditKind::GrantConsumed,
            audit_bytes(&payload),
        );

        Ok(())
    }

    fn substitute_cookies(
        &self,
        grant: GrantId,
        session: SessionId,
    ) -> Result<Zeroizing<Vec<u8>>, LoomError> {
        // Resolve grant under read lock — TOCTOU-free clone of fields.
        // Session binding (D5 / council FND-0008) is structural now: the map
        // key is (session, grant). Cross-session use of an id that exists
        // under another session keeps the typed `vault_session_mismatch`
        // envelope for the MCP error surface.
        let (label, issued_at_ms, ttl_ms, revoked) = {
            let grants = self.grants.read();
            match grants.get(&(session.clone(), grant.clone())) {
                Some(g) => (g.label.clone(), g.issued_at_ms, g.ttl_ms, g.revoked),
                None => {
                    let other_session = grants
                        .keys()
                        .find(|(_, gid)| gid == &grant)
                        .map(|(s, _)| s.clone());
                    return Err(match other_session {
                        Some(expected) => {
                            LoomError::new(LoomErrorCode::VaultRejection, "vault-session-mismatch")
                                .with_context(serde_json::json!({
                                    "code": "vault_session_mismatch",
                                    "details": {
                                        "expected_session": expected.0,
                                        "observed_session": session.0,
                                    }
                                }))
                        }
                        None => {
                            LoomError::new(LoomErrorCode::VaultRejection, "vault-grant-not-found")
                        }
                    });
                }
            }
        };

        let now = now_ms();

        // Check 1: Revoked
        if revoked {
            return Err(LoomError::new(
                LoomErrorCode::VaultGrantRevoked,
                "cookie grant revoked",
            ));
        }

        // Check 2: TTL
        if now > issued_at_ms.saturating_add(ttl_ms) {
            return Err(LoomError::new(
                LoomErrorCode::VaultGrantExpired,
                "cookie grant ttl exceeded",
            ));
        }

        // Fetch keychain bytes — Zeroizing<Vec<u8>> drops at function exit.
        let secret: Zeroizing<Vec<u8>> = self
            .keychain
            .get_secret(&label)
            .map_err(from_keychain_err)?;

        // Emit CookiesSubstituted audit. Per D5, the audit chain includes
        // cookie *names* (for replay determinism) but NOT *values*. Extracting
        // names requires parsing the JSON blob; we do that without holding
        // raw value bytes after the parse — names go to the audit, the blob
        // is returned to the caller which decodes once at the CDP boundary.
        // The per-run session id is deliberately NOT in the payload: these
        // bytes are chain-hashed and `hashable_line()` cannot project values
        // inside the canonical_bytes number array — the same exclusion it
        // applies to the Header's top-level session_id (NFR-DET-01).
        let cookie_names = extract_cookie_names(&secret);
        let payload = serde_json::json!({
            "grant_id": grant.0,
            "cookie_names": cookie_names,
        });
        let audit_payload = serde_jcs::to_string(&payload)
            .unwrap_or_else(|_| serde_json::to_string(&payload).unwrap_or_default())
            .into_bytes();
        let _ = self.manifest_writer.append_audit(
            session,
            AuditKind::CookiesSubstituted,
            audit_payload,
        );

        Ok(secret)
    }

    fn revoke(&self, grant: GrantId, reason: RevokeReason) -> Result<(), LoomError> {
        // The public revoke surface (vault.revoke RPC) addresses grants by
        // id only. Deterministic ids can repeat across same-seed sessions —
        // revoke is an operator kill-switch, so EVERY matching entry is
        // revoked (each audited on its own session chain); normally one.
        let keys: Vec<(SessionId, GrantId)> = {
            let grants = self.grants.read();
            grants
                .keys()
                .filter(|(_, gid)| gid == &grant)
                .cloned()
                .collect()
        };
        if keys.is_empty() {
            return Err(LoomError::new(
                LoomErrorCode::VaultRejection,
                "vault-grant-not-found",
            ));
        }
        for key in &keys {
            self.revoke_entry(key);
        }
        drop(reason);
        Ok(())
    }

    fn begin_session(&self, session: &SessionId, seed: u64) {
        // Idempotent: keep the existing context if the session was already
        // registered (the rng/tick sequence must not restart mid-session).
        self.det
            .lock()
            .entry(session.clone())
            .or_insert_with(|| VaultSessionCtx {
                rng: vault_session_rng(&seed.to_le_bytes()),
                tick: 0,
            });
    }

    fn end_session(&self, session: &SessionId) {
        self.det.lock().remove(session);
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
            .map(|((_, gid), g)| GrantSnapshot {
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
        // Full (session, grant) keys so the cascade revokes EXACTLY the
        // label-referencing entries — never a same-id grant for a different
        // label under another same-seed session.
        let now = now_ms();
        let referencing_grants: Vec<(SessionId, GrantId)> = {
            let grants = self.grants.read();
            grants
                .iter()
                .filter(|(_, g)| {
                    g.label == label && !g.revoked && now < g.issued_at_ms.saturating_add(g.ttl_ms)
                })
                .map(|(key, _)| key.clone())
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
            for key in &referencing_grants {
                self.revoke_entry(key);
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
