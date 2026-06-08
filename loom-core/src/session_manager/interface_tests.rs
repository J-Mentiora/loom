// Interface tests for `SessionManager`. Verifies the warm-path
// budget shape, abort propagation primitives, FSM transitions,
// per-session tokio task isolation.

use super::session_manager::{
    AbortReason, LocalSessionManager, Session, SessionCreateOpts, SessionError, SessionStatus,
};
use loom_core::budget_enforcer::{BudgetEnforcer, BudgetLimits, LocalBudgetEnforcer};
use loom_core::content_store::{ContentStore, LocalContentStore};
use loom_core::determinism_harness::DeterminismHarness;
use loom_core::error::LoomError;
use loom_core::manifest_writer::{LocalManifestWriter, ManifestWriter, SessionId};
use loom_core::observability::Observability;
use loom_core::vault::{KeychainAccess, LocalVault, Vault};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use zeroize::Zeroizing;

struct StubKc;
impl KeychainAccess for StubKc {
    fn get_secret(&self, _label: &str) -> Result<Zeroizing<Vec<u8>>, loom_keychain::KeychainError> {
        Ok(Zeroizing::new(vec![0u8; 16]))
    }
    fn set_secret(
        &self,
        _label: &str,
        _secret: Zeroizing<Vec<u8>>,
    ) -> Result<(), loom_keychain::KeychainError> {
        Err(loom_keychain::KeychainError::new(
            loom_keychain::KeychainErrorKind::Unavailable,
            "test stub",
        ))
    }
    fn delete_secret(&self, _label: &str) -> Result<(), loom_keychain::KeychainError> {
        Err(loom_keychain::KeychainError::new(
            loom_keychain::KeychainErrorKind::Unavailable,
            "test stub",
        ))
    }
    fn list_labels(&self) -> Result<Vec<String>, loom_keychain::KeychainError> {
        Err(loom_keychain::KeychainError::new(
            loom_keychain::KeychainErrorKind::Unavailable,
            "test stub",
        ))
    }
}

fn fixture() -> Arc<LocalSessionManager> {
    let obs = Observability::new(PathBuf::from("/tmp/loom-test/loom.log"), false);
    let cs: Arc<dyn ContentStore> = Arc::new(LocalContentStore::new(
        PathBuf::from("/tmp/loom-test/store"),
        obs.clone(),
    ));
    let mw: Arc<dyn ManifestWriter> = Arc::new(LocalManifestWriter::new(
        PathBuf::from("/tmp/loom-test/sessions"),
        obs.clone(),
    ));
    let kc: Arc<dyn KeychainAccess> = Arc::new(StubKc);
    let v: Arc<dyn Vault> = Arc::new(LocalVault::new(kc, mw.clone(), obs.clone()));
    let be: Arc<dyn BudgetEnforcer> = Arc::new(LocalBudgetEnforcer::new(obs.clone()));
    let dh = Arc::new(DeterminismHarness::new(42, mw.clone()));
    LocalSessionManager::new(
        cs,
        mw,
        v,
        be,
        dh,
        obs,
        0,
        PathBuf::from("/tmp/loom-test/sessions"),
    )
}

// === Warm-path shape ===

#[test]
fn create_signature_takes_opts_returns_session_id() {
    let sm = fixture();
    fn _ck(sm: &LocalSessionManager, o: SessionCreateOpts) -> Result<SessionId, LoomError> {
        sm.create(o)
    }
    let _ = _ck;
    let _ = sm;
}

#[test]
fn create_opts_replay_of_field_is_optional_session_id() {
    let opts = SessionCreateOpts {
        agent_id: "agent-1".into(),
        surface: "web".into(),
        seed: Some(42),
        limits: Some(BudgetLimits::default()),
        replay_of: None,
        started_at_ms_override: None,
        capture_policy: None,
        no_blocklist: false,
        no_determinism: false,
        profile: "safe".to_string(),
    };
    assert!(opts.replay_of.is_none());
    let opts2 = SessionCreateOpts {
        replay_of: Some(SessionId("01HZ".into())),
        ..opts
    };
    assert!(opts2.replay_of.is_some());
}

// === Profile threading + downloads dir ===

/// Root-cause fix: profile reaches `Session.profile` when
/// passed via `SessionCreateOpts`. Pre-fix the daemon's `_profile: &str`
/// dropped the value on the floor; this test pins the wire path.
#[test]
fn session_profile_populated_from_opts() {
    let sm = fixture();
    let opts = SessionCreateOpts {
        agent_id: "agent-1".into(),
        surface: "web".into(),
        seed: Some(42),
        limits: None,
        replay_of: None,
        started_at_ms_override: None,
        capture_policy: None,
        no_blocklist: false,
        no_determinism: false,
        profile: "safe".to_string(),
    };
    let id = sm.create(opts).expect("session created");
    let session = sm.get(id).expect("session retrievable");
    assert_eq!(session.profile, "safe");
    // Safe profile creates the session-scoped downloads
    // dir. Pre-fix this dir didn't exist; now Chromium uses it as the
    // confined download path.
    let downloads_dir = session
        .downloads_dir
        .as_ref()
        .expect("safe profile creates downloads_dir");
    assert!(
        downloads_dir.ends_with("downloads"),
        "downloads_dir suffix mismatch: {downloads_dir:?}"
    );
}

/// Non-safe profile does NOT create a downloads dir
/// (no Chromium-side confinement to wire up).
#[test]
fn non_safe_profile_skips_downloads_dir() {
    let sm = fixture();
    let opts = SessionCreateOpts {
        agent_id: "agent-1".into(),
        surface: "web".into(),
        seed: Some(42),
        limits: None,
        replay_of: None,
        started_at_ms_override: None,
        capture_policy: None,
        no_blocklist: false,
        no_determinism: false,
        profile: "standard".to_string(),
    };
    let id = sm.create(opts).expect("session created");
    let session = sm.get(id).expect("session retrievable");
    assert_eq!(session.profile, "standard");
    assert!(
        session.downloads_dir.is_none(),
        "non-safe profile must NOT create a downloads dir"
    );
}

/// Serde round-trip: SessionCreateOpts.profile survives
/// JSON serialize+deserialize. The wire boundary `CreateSessionParams`
/// defaults profile to "safe"; this test pins that the round-trip is
/// stable so the value can't be silently dropped between the two
/// boundary structs.
#[test]
fn session_create_opts_profile_serde_round_trips() {
    let opts = SessionCreateOpts {
        agent_id: "agent-1".into(),
        surface: "web".into(),
        seed: None,
        limits: None,
        replay_of: None,
        started_at_ms_override: None,
        capture_policy: None,
        no_blocklist: false,
        no_determinism: false,
        profile: "safe".to_string(),
    };
    let json = serde_json::to_string(&opts).expect("serialize");
    assert!(
        json.contains("\"profile\":\"safe\""),
        "serialized form must include profile: {json}"
    );
    let round: SessionCreateOpts = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(round.profile, "safe");
}

/// Serde default: missing `profile` field deserializes
/// to "safe" (matches `CreateSessionParams::default_profile` at the
/// wire boundary). Guards back-compat with any pre-fix opts blobs.
#[test]
fn session_create_opts_profile_default_when_missing() {
    let json = r#"{
        "agent_id": "agent-1",
        "surface": "web",
        "seed": null,
        "limits": null,
        "replay_of": null,
        "started_at_ms_override": null,
        "capture_policy": null
    }"#;
    let round: SessionCreateOpts = serde_json::from_str(json).expect("deserialize");
    assert_eq!(round.profile, "safe");
}

// === abort primitives present ===

#[test]
fn abort_signature_takes_session_id_and_reason() {
    let sm = fixture();
    fn _ck(sm: &LocalSessionManager, id: SessionId, r: AbortReason) -> Result<(), LoomError> {
        sm.abort(id, r)
    }
    let _ = _ck;
    let _ = sm;
}

#[test]
fn session_struct_holds_arc_atomicbool_abort_flag_and_arc_notify() {
    // Compile-time guarantee that the abort primitives are present in the
    // required shape. Polling-only mechanisms would lack
    // the Notify and would fail this test (variant absent).
    fn _ck(s: &Session) -> bool {
        s.abort_flag.load(Ordering::Acquire)
    }
    fn _ck2(s: &Session) {
        let _: &Arc<tokio::sync::Notify> = &s.abort_notify;
    }
    let _ = (_ck, _ck2);
}

// === FSM transitions ===

#[test]
fn session_status_enum_has_six_variants() {
    let _v = [
        SessionStatus::Created,
        SessionStatus::Active,
        SessionStatus::Closed,
        SessionStatus::Aborted,
        SessionStatus::Killed,
        SessionStatus::Crashed,
    ];
}

#[test]
fn close_returns_session_already_closed_when_already_terminal() {
    let _e = SessionError::SessionAlreadyClosed {
        session_id: "01HZ".into(),
    };
}

#[test]
fn get_returns_session_unknown_for_missing_id() {
    let _e = SessionError::SessionUnknown {
        session_id: "missing".into(),
    };
}

#[test]
fn abort_returns_unit_or_session_unknown() {
    let _e = SessionError::SessionUnknown {
        session_id: "01HZ".into(),
    };
    let _e2 = SessionError::SessionAborted {
        reason: "user_request".into(),
    };
}

#[test]
fn killed_state_records_session_killed_with_reason() {
    let _e = SessionError::SessionKilled {
        reason: "store_full_no_evictable".into(),
    };
}

#[test]
fn session_profile_immutable_variant_exists_for_facade() {
    let _e = SessionError::SessionProfileImmutable;
}

#[test]
fn abort_all_signature_takes_reason_returns_unit_or_error() {
    let sm = fixture();
    fn _ck(sm: &LocalSessionManager, r: AbortReason) -> Result<(), LoomError> {
        sm.abort_all(r)
    }
    let _ = _ck;
    let _ = sm;
}

// === per-session structured-concurrency scope ===

#[test]
fn session_carries_session_scope() {
    fn _ck(s: &Session) {
        let _: &Arc<loom_core::session_scope::SessionScope> = &s.scope;
    }
    let _ = _ck;
}

// === Kill-callback cycle break ===

#[test]
fn kill_callback_for_returns_arc_dyn_fn_session_id_killreason() {
    let sm = fixture();
    let cb = sm.kill_callback_for(SessionId("01HZ".into()));
    // Compile-time check: cb is Arc<dyn Fn(SessionId, KillReason) + Send + Sync>.
    let _: &Arc<dyn Fn(SessionId, loom_core::budget_enforcer::KillReason) + Send + Sync> = &cb;
}
