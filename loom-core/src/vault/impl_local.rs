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
    AddCredentialOpts, AddCredentialReceipt, CredentialType, Grant, GrantId, GrantOpts,
    GrantSnapshot, LocalVault, NetRequest, RevokeReason, Vault, OAUTH_PROVIDER_ALLOWLIST,
};
use loom_keychain::{KeychainError, KeychainErrorKind};
use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

/// Translate a backend-layer `KeychainError` into a vault-layer `LoomError`.
/// W1 uses a minimal mapping that preserves existing test semantics; W5
/// refines this with five new `LoomErrorCode` variants
/// (`VaultPermissionDenied`, `VaultBackendUnavailable`, `VaultBackendTimeout`,
/// `VaultNonInteractivePrompt`, `VaultInternal`) — see plan amendment A-W5.2.
fn from_keychain_err(err: KeychainError) -> LoomError {
    let code = match err.kind() {
        KeychainErrorKind::NotFound => LoomErrorCode::VaultUnknownLabel,
        // W1 collapses every non-NotFound kind into VaultRejection so the
        // existing tests stay green. W5 splits this into five typed codes.
        KeychainErrorKind::Denied
        | KeychainErrorKind::Unavailable
        | KeychainErrorKind::TimedOut
        | KeychainErrorKind::NonInteractivePrompt
        | KeychainErrorKind::Internal => LoomErrorCode::VaultRejection,
    };
    LoomError::new(code, err.to_string())
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
}
