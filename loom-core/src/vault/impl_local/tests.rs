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

    fn set_secret(&self, _label: &str, _secret: Zeroizing<Vec<u8>>) -> Result<(), KeychainError> {
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
        authorization: None,
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
    let err = vault.substitute(&sid, gid, &mut r).unwrap_err();
    assert_eq!(err.code, LoomErrorCode::VaultRejection);
    let ctx = err.context.unwrap();
    assert_eq!(ctx["code"], "vault_origin_mismatch");
    assert_eq!(ctx["details"]["expected_origin"], TEST_ORIGIN);
    assert_eq!(ctx["details"]["observed_origin"], "api.gitlab.com");
    assert!(r.authorization.is_none(), "no token on rejection");
}

// ── Scope escalation rejection ─────────────────────────

#[test]
fn substitute_rejects_scope_escalation() {
    let (vault, _mw, sid) = fixture();
    let gid = vault.grant(sid.clone(), default_opts()).unwrap();
    let mut r = net_req(TEST_ORIGIN, &["repo:read", "repo:write"]);
    let err = vault.substitute(&sid, gid, &mut r).unwrap_err();
    assert_eq!(err.code, LoomErrorCode::VaultRejection);
    let ctx = err.context.unwrap();
    assert_eq!(ctx["code"], "vault_scope_insufficient");
    assert_eq!(ctx["details"]["required_scope"], "repo:write");
    assert!(r.authorization.is_none());
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
            (sid.clone(), gid.clone()),
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
    let err = vault.substitute(&sid, gid, &mut r).unwrap_err();
    assert_eq!(err.code, LoomErrorCode::VaultGrantExpired);
    let ctx = err.context.unwrap();
    assert_eq!(ctx["code"], "vault_grant_expired");
    assert!(ctx["details"]["expired_at"].as_u64().unwrap() == 1);
    assert!(ctx["details"]["observed_at"].as_u64().unwrap() > 1);
    assert!(r.authorization.is_none());
}

// ── substitute() success path ─────────────────────────────────────────

#[test]
fn substitute_writes_authorization_header() {
    let (vault, _mw, sid) = fixture();
    let gid = vault.grant(sid.clone(), default_opts()).unwrap();
    let mut r = net_req(TEST_ORIGIN, &["repo:read"]);
    assert!(r.authorization.is_none());
    vault.substitute(&sid, gid, &mut r).unwrap();
    let auth = r.authorization.as_ref().expect("Authorization must be set");
    assert!(
        auth.expose().starts_with("Bearer "),
        "must be a Bearer token"
    );
    assert!(
        !r.headers.contains_key("Authorization"),
        "token must not sit in the Debug/Serialize-able headers map"
    );
}

// ── G1/TB4: bearer token never reaches Debug/Serialize ──────────────

#[test]
fn debug_and_serialize_of_substituted_request_redact_token() {
    let (vault, _mw, sid) = fixture();
    let gid = vault.grant(sid.clone(), default_opts()).unwrap();
    let mut r = net_req(TEST_ORIGIN, &["repo:read"]);
    vault.substitute(&sid, gid, &mut r).unwrap();

    let secret_str = std::str::from_utf8(TEST_SECRET).unwrap();
    let dbg = format!("{r:?}");
    assert!(!dbg.contains(secret_str), "Debug leaks the token: {dbg}");
    assert!(dbg.contains("[REDACTED]"), "Debug shows redaction marker");

    let json = serde_json::to_string(&r).unwrap();
    assert!(!json.contains(secret_str), "Serialize leaks the token");
    assert!(
        !json.contains("authorization"),
        "#[serde(skip)] must omit the slot entirely"
    );
}

#[test]
fn substituted_authorization_wire_value_is_preserved() {
    // Wire behavior is identical: loom-host's do_http_request sends
    // exactly "Bearer <secret>" from the redacted slot.
    let (vault, _mw, sid) = fixture();
    let gid = vault.grant(sid.clone(), default_opts()).unwrap();
    let mut r = net_req(TEST_ORIGIN, &["repo:read"]);
    vault.substitute(&sid, gid, &mut r).unwrap();
    let auth = r.authorization.as_ref().expect("authorization set");
    assert_eq!(auth.expose().as_str(), "Bearer secret-token-bytes");
}

#[test]
fn substitute_returns_unit_not_secret() {
    let (vault, _mw, sid) = fixture();
    let gid = vault.grant(sid.clone(), default_opts()).unwrap();
    let mut r = net_req(TEST_ORIGIN, &["repo:read"]);
    // Return type is () — no secret in return value
    let result: Result<(), LoomError> = vault.substitute(&sid, gid, &mut r);
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
    let err = vault.substitute(&sid, gid, &mut r).unwrap_err();
    assert_eq!(err.code, LoomErrorCode::VaultGrantRevoked);
    assert!(r.authorization.is_none());
}

// ── Audit entries in order ────────────────────────────

#[test]
fn audit_entries_in_order_issued_consumed_revoked() {
    let (vault, mw, sid) = fixture();
    let gid = vault.grant(sid.clone(), default_opts()).unwrap();
    let mut r = net_req(TEST_ORIGIN, &["repo:read"]);
    vault.substitute(&sid, gid.clone(), &mut r).unwrap();
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
    vault.substitute(&sid, gid, &mut r).unwrap();

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
    vault.substitute(&sid, gid, &mut r).unwrap();

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

// ── NFR-DET-01: audit canonical bytes are cross-run deterministic ──

fn audit_canonical_bytes(entry: &Value) -> Vec<u8> {
    entry["canonical_bytes"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_u64().map(|n| n as u8))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn same_seed_sessions_produce_identical_audit_canonical_bytes() {
    // ts_tick (wall clock) and grant ids (random ULIDs) used to poison
    // chain-hashed canonical_bytes. Same seed + same flow must now give
    // byte-identical payloads despite different random session ids.
    let run = || {
        let (vault, mw, sid) = fixture();
        vault.begin_session(&sid, 42);
        let gid = vault.grant(sid.clone(), default_opts()).unwrap();
        let mut ok = net_req(TEST_ORIGIN, &["repo:read"]);
        vault.substitute(&sid, gid.clone(), &mut ok).unwrap();
        // A denied attempt exercises the GrantRejected payload too.
        let mut denied = net_req("api.gitlab.com", &["repo:read"]);
        let _ = vault
            .substitute(&sid, gid.clone(), &mut denied)
            .unwrap_err();
        let reason = RevokeReason {
            reason: "test".to_string(),
        };
        vault.revoke(gid, reason).unwrap();
        read_audit_entries(&mw, &sid)
            .iter()
            .map(audit_canonical_bytes)
            .collect::<Vec<_>>()
    };
    let (a, b) = (run(), run());
    assert!(a.len() >= 4, "issued/consumed/rejected/revoked expected");
    assert_eq!(a, b, "same-seed sessions must emit identical audit bytes");
    // ts_tick is the 0-based session-relative vault event clock — never
    // wall-clock ms.
    let ticks: Vec<u64> = a
        .iter()
        .filter_map(|bytes| serde_json::from_slice::<Value>(bytes).ok())
        .filter_map(|p| p["ts_tick"].as_u64())
        .collect();
    assert_eq!(ticks, vec![0, 1, 2, 3]);
}

#[test]
fn cookie_audit_omits_per_run_session_id_and_is_seed_stable() {
    let run = || {
        let (vault, mw, sid) = fixture();
        vault.begin_session(&sid, 42);
        let mut opts = default_opts();
        opts.credential_type = CredentialType::Cookie;
        let gid = vault.grant(sid.clone(), opts).unwrap();
        // StubKeychain's blob is not cookie-JSON → cookie_names = [].
        let _ = vault.substitute_cookies(gid, sid.clone()).unwrap();

        let entries = read_audit_entries(&mw, &sid);
        let cookie_entry = entries
            .iter()
            .find(|e| e["audit_kind"] == "cookies_substituted")
            .expect("CookiesSubstituted audit");
        let payload = audit_payload_from_entry(cookie_entry).expect("payload parses");
        assert!(
            payload.get("session_id").is_none(),
            "per-run session_id must not be in chain-hashed cookie audit bytes: {payload}"
        );
        (audit_canonical_bytes(cookie_entry), sid)
    };
    let (bytes_a, sid_a) = run();
    let (bytes_b, sid_b) = run();
    assert_ne!(sid_a, sid_b, "fixture mints distinct session ids");
    assert_eq!(bytes_a, bytes_b, "cookie audit bytes must be seed-stable");
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
            (sid.clone(), gid.clone()),
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
    vault.substitute(&sid, gid.clone(), &mut r1).unwrap();
    vault.substitute(&sid, gid, &mut r2).unwrap();
    assert!(r1.authorization.is_some());
    assert!(r2.authorization.is_some());
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
            (
                sid.clone(),
                GrantId("01HZACTIVEGRANTAAAAAAAAAAAA".to_string()),
            ),
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
        .get(&(
            sid.clone(),
            GrantId("01HZACTIVEGRANTAAAAAAAAAAAA".to_string()),
        ))
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
    let stored_count = kinds
        .iter()
        .filter(|k| k.as_str() == "secret_stored")
        .count();
    let replaced_count = kinds
        .iter()
        .filter(|k| k.as_str() == "secret_replaced")
        .count();
    assert_eq!(stored_count, 2, "two stores → two secret_stored audits");
    assert_eq!(
        replaced_count, 1,
        "second store → one secret_replaced audit"
    );
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
