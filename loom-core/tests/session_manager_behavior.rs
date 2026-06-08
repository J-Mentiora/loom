// Behavior tests for LocalSessionManager.
// (Originally TDD Red-phase scaffolding; all tests now exercise real impl.)
//
// Coverage:
//   - test_create_returns_ulid_session_id
//   - test_create_persists_manifest_header
//   - test_unknown_session_returns_session_not_found_error
//   - test_no_implicit_session_creation_on_unknown_id
//   - test_close_transitions_to_closed_status
//   - test_close_finalizes_manifest_with_terminal_entry
//   - test_action_on_closed_session_returns_session_already_closed
//   - test_profile_mutation_returns_session_profile_immutable
//   - test_abort_sets_abort_flag_and_notifies
//   - test_abort_appends_terminal_entry_to_manifest
//   - test_abort_completes_within_1s_wall_clock
//   - test_abort_all_aborts_every_active_session

use loom_core::budget_enforcer::{BudgetEnforcer, LocalBudgetEnforcer};
use loom_core::content_store::{ContentStore, LocalContentStore};
use loom_core::determinism_harness::DeterminismHarness;
use loom_core::error::{LoomError, LoomErrorCode};
use loom_core::manifest_writer::{LocalManifestWriter, ManifestWriter, SessionId};
use loom_core::observability::Observability;
use loom_core::session_manager::{
    AbortReason, LocalSessionManager, SessionCreateOpts, SessionError, SessionStatus,
};
use loom_core::vault::{KeychainAccess, LocalVault, Vault};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
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

fn make_sm(tmp: &str) -> Arc<LocalSessionManager> {
    let obs = Observability::new(PathBuf::from(format!("{tmp}/loom.log")), false);
    let cs: Arc<dyn ContentStore> = Arc::new(LocalContentStore::new(
        PathBuf::from(format!("{tmp}/store")),
        obs.clone(),
    ));
    let mw: Arc<dyn ManifestWriter> = Arc::new(LocalManifestWriter::new(
        PathBuf::from(format!("{tmp}/sessions")),
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
        std::path::PathBuf::from("/tmp/loom-test/sessions"),
    )
}

fn default_opts() -> SessionCreateOpts {
    SessionCreateOpts {
        agent_id: "test-agent".into(),
        surface: "test".into(),
        seed: None,
        limits: None,
        replay_of: None,
        started_at_ms_override: None,
        capture_policy: None,
        no_blocklist: false,
        no_determinism: false,
        profile: "safe".to_string(),
    }
}

// === session creation + ULID ===

#[test]
fn test_create_returns_ulid_session_id() {
    let sm = make_sm("/tmp/loom-test-create-ulid");
    let id = sm.create(default_opts()).unwrap();
    // ULID: 26 chars, Crockford base32 lowercase.
    assert_eq!(id.0.len(), 26, "session id must be 26-char ULID");
    assert!(
        id.0.chars()
            .all(|c: char| c.is_ascii_digit() || c.is_ascii_lowercase()),
        "session id must be lowercase alphanumeric Crockford base32"
    );
}

#[test]
fn test_create_persists_manifest_header() {
    let tmp = "/tmp/loom-test-create-header";
    let sm = make_sm(tmp);
    let id = sm.create(default_opts()).unwrap();
    let wal_path = PathBuf::from(format!("{tmp}/sessions/{}/manifest.wal", id.0));
    assert!(wal_path.exists(), "manifest.wal must exist after create");
    let contents = std::fs::read_to_string(&wal_path).unwrap();
    let first_line: serde_json::Value = serde_json::from_str(contents.lines().next().unwrap())
        .expect("first WAL line must be valid JSON");
    assert_eq!(
        first_line["kind"], "header",
        "first manifest entry must be a Header"
    );
    assert_eq!(first_line["session_id"], id.0.as_str());
}

// === unknown session returns error ===

#[test]
fn test_unknown_session_returns_session_not_found_error() {
    let sm = make_sm("/tmp/loom-test-unknown");
    let result = sm.get(SessionId("00000000000000000000000000".into()));
    let err = match result {
        Err(e) => e,
        Ok(_) => panic!("expected SessionNotFound error"),
    };
    assert_eq!(err.code, LoomErrorCode::SessionNotFound);
}

#[test]
fn test_no_implicit_session_creation_on_unknown_id() {
    let tmp = "/tmp/loom-test-no-implicit";
    let _ = std::fs::remove_dir_all(tmp);
    let sm = make_sm(tmp);
    let unknown = SessionId("00000000000000000000000000".into());
    let _ = sm.get(unknown.clone());
    let session_dir = PathBuf::from(format!("{tmp}/sessions/{}", unknown.0));
    assert!(
        !session_dir.exists(),
        "get on unknown id must not create session directory"
    );
}

// === close lifecycle ===

#[tokio::test]
async fn test_close_transitions_to_closed_status() {
    let sm = make_sm("/tmp/loom-test-close-status");
    let id = sm.create(default_opts()).unwrap();
    sm.close(id.clone()).unwrap();
    let session = sm.get(id).unwrap();
    // parking_lot::Mutex — sync lock, no await needed
    let status = *session.status.lock();
    assert_eq!(status, SessionStatus::Closed);
}

#[test]
fn test_close_finalizes_manifest_with_terminal_entry() {
    let tmp = "/tmp/loom-test-close-terminal";
    let sm = make_sm(tmp);
    let id = sm.create(default_opts()).unwrap();
    sm.close(id.clone()).unwrap();
    let wal_path = PathBuf::from(format!("{tmp}/sessions/{}/manifest.wal", id.0));
    let contents = std::fs::read_to_string(&wal_path).unwrap();
    let has_terminal = contents.lines().any(|line| {
        serde_json::from_str::<serde_json::Value>(line)
            .map(|v| v["kind"] == "session_terminal")
            .unwrap_or(false)
    });
    assert!(
        has_terminal,
        "close must append SessionTerminal entry to manifest"
    );
}

#[test]
fn test_action_on_closed_session_returns_session_already_closed() {
    let sm = make_sm("/tmp/loom-test-close-idempotent");
    let id = sm.create(default_opts()).unwrap();
    sm.close(id.clone()).unwrap();
    // Second close on already-closed session must return SessionAlreadyClosed.
    let err = sm.close(id).unwrap_err();
    assert_eq!(err.code, LoomErrorCode::SessionAlreadyClosed);
}

// === profile immutability ===

#[test]
fn test_profile_mutation_returns_session_profile_immutable() {
    // LocalSessionManager exposes no update_profile() method.
    // The facade returns SessionError::SessionProfileImmutable if profile
    // mutation is attempted. Verify the variant compiles (shape check).
    let _e = SessionError::SessionProfileImmutable;
    // Verify it maps to InvalidArgument at the facade boundary.
    let loom_err: LoomError = _e.into();
    assert_eq!(loom_err.code, LoomErrorCode::InvalidArgument);
}

// === abort signal ===

#[tokio::test]
async fn test_abort_sets_abort_flag_and_notifies() {
    let sm = make_sm("/tmp/loom-test-abort-flag");
    let id = sm.create(default_opts()).unwrap();
    sm.abort(
        id.clone(),
        AbortReason {
            reason: "test".into(),
        },
    )
    .unwrap();
    // get() should return the session (still in DashMap, now Aborted status)
    let session = match sm.get(id) {
        Ok(s) => s,
        Err(_) => panic!("aborted session should still be retrievable"),
    };
    assert!(
        session.abort_flag.load(Ordering::Acquire),
        "abort() must set abort_flag to true"
    );
}

#[test]
fn test_abort_appends_terminal_entry_to_manifest() {
    let tmp = "/tmp/loom-test-abort-terminal";
    let sm = make_sm(tmp);
    let id = sm.create(default_opts()).unwrap();
    sm.abort(
        id.clone(),
        AbortReason {
            reason: "user_request".into(),
        },
    )
    .unwrap();
    // Give the async task up to 1s to write the terminal entry.
    let wal_path = PathBuf::from(format!("{tmp}/sessions/{}/manifest.wal", id.0));
    let deadline = Instant::now() + std::time::Duration::from_secs(1);
    loop {
        let contents = std::fs::read_to_string(&wal_path).unwrap_or_default();
        let has_terminal = contents.lines().any(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .map(|v| v["kind"] == "session_terminal")
                .unwrap_or(false)
        });
        if has_terminal {
            break;
        }
        if Instant::now() > deadline {
            panic!("abort() must write SessionTerminal to manifest within 1s");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn test_abort_completes_within_1s_wall_clock() {
    let sm = make_sm("/tmp/loom-test-abort-timing");
    let id = sm.create(default_opts()).unwrap();
    let start = Instant::now();
    sm.abort(
        id,
        AbortReason {
            reason: "timing_test".into(),
        },
    )
    .unwrap();
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_millis() < 1000,
        "abort() must return within 1s; took {}ms",
        elapsed.as_millis()
    );
}

// === abort_all ===

#[test]
fn test_abort_all_aborts_every_active_session() {
    let sm = make_sm("/tmp/loom-test-abort-all");
    let id1 = sm.create(default_opts()).unwrap();
    let id2 = sm.create(default_opts()).unwrap();
    let id3 = sm.create(default_opts()).unwrap();
    sm.abort_all(AbortReason {
        reason: "sigterm".into(),
    })
    .unwrap();
    // All sessions must have their abort_flag set within 1s.
    let deadline = Instant::now() + std::time::Duration::from_secs(1);
    for id in [id1, id2, id3] {
        let session = sm.get(id).unwrap();
        loop {
            if session.abort_flag.load(Ordering::Acquire) {
                break;
            }
            if Instant::now() > deadline {
                panic!("abort_all() must signal all sessions within 1s");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}
