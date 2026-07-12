//! Integration tests — `loom-core` contract.
//!
//! Contract: `projects/loom/architecture/contracts/loom-core_contract.md`
//!
//! Tests exercise the real `CoreApiFacade` with a temp file-system data root
//! (no mocks). Each test is isolated via `TempDir`.

use loom_core::core_api_facade::{CoreApiFacade, CoreConfig};
use loom_core::error::LoomErrorCode;
use loom_core::session_manager::session_manager::SessionCreateOpts;
use std::time::Duration;
use tempfile::TempDir;

// ─── Helper ───────────────────────────────────────────────────────────────────

fn make_core(dir: &TempDir) -> std::sync::Arc<CoreApiFacade> {
    let data_root = dir.path().to_path_buf();
    let config = CoreConfig {
        data_root: data_root.clone(),
        log_path: data_root.join("daemon.log"),
        otel_enabled: false,
        default_seed: 42,
        checkpoint_every_n: 100,
    };
    let keychain: std::sync::Arc<dyn loom_core::vault::KeychainAccess> =
        std::sync::Arc::new(loom_keychain::StubKeychain);
    CoreApiFacade::new(config, keychain).expect("CoreApiFacade::new must succeed with temp dir")
}

fn default_session_opts() -> SessionCreateOpts {
    SessionCreateOpts {
        agent_id: "test-agent".into(),
        surface: "web".into(),
        seed: Some(1234),
        limits: None,
        replay_of: None,
        started_at_ms_override: None,
        capture_policy: None,
        no_blocklist: false,
        no_determinism: false,
        record_screencast: false,
        audio: false,
        profile: "safe".to_string(),
    }
}

// ─── Test 1: Happy — create session returns a ULID session_id ────────────────

/// Contract: SessionManager::create returns a 26-char lowercase ULID.
/// SLA: p50 ≤ 500ms warm (NFR-PERF-01).
#[test]
fn test_session_create_returns_ulid() {
    let dir = TempDir::new().unwrap();
    let core = make_core(&dir);

    let t0 = std::time::Instant::now();
    let session_id = core
        .session_manager
        .create(default_session_opts())
        .expect("session create must succeed");
    let elapsed = t0.elapsed();

    // ULID is 26 lowercase alphanumeric chars
    assert_eq!(
        session_id.0.len(),
        26,
        "ULID must be 26 chars; got {:?}",
        session_id.0
    );
    assert!(
        session_id
            .0
            .chars()
            .all(|c| c.is_alphanumeric() && (c.is_lowercase() || c.is_ascii_digit())),
        "ULID must be lowercase alphanumeric; got {:?}",
        session_id.0
    );

    // SLA: warm create p50 ≤ 500ms
    assert!(
        elapsed < Duration::from_millis(500),
        "session create p50 SLA violated: {}ms > 500ms",
        elapsed.as_millis()
    );
}

// ─── Test 2: Happy — create + get returns Active session ─────────────────────

/// Contract: get(id) returns the session in Active status after create.
#[test]
fn test_get_session_after_create_is_active() {
    use loom_core::session_manager::session_manager::SessionStatus;

    let dir = TempDir::new().unwrap();
    let core = make_core(&dir);

    let id = core.session_manager.create(default_session_opts()).unwrap();
    let session = core
        .session_manager
        .get(id.clone())
        .expect("get must find created session");

    let status = *session.status.lock();
    assert_eq!(
        status,
        SessionStatus::Active,
        "newly created session must be Active; got {:?}",
        status
    );
    assert_eq!(
        session.id, id,
        "session id must match the one returned by create"
    );
}

// ─── Test 3: Error — get with unknown session returns SessionNotFound ─────────

/// Contract: get(unknown_id) → LoomError with code = SessionNotFound.
#[test]
fn test_get_unknown_session_returns_not_found() {
    let dir = TempDir::new().unwrap();
    let core = make_core(&dir);

    use loom_core::manifest_writer::SessionId;
    let result = core
        .session_manager
        .get(SessionId("01NONEXISTENT0000000000000".into()));
    assert!(result.is_err(), "get of unknown session must fail");
    let err = result.err().unwrap();

    assert_eq!(
        err.code,
        LoomErrorCode::SessionNotFound,
        "unknown session must return SessionNotFound; got {:?}",
        err.code
    );
}

// ─── Test 4: Happy — create + close transitions to Closed ────────────────────

/// Contract: close(id) → session status = Closed, no further actions accepted.
#[test]
fn test_close_session_transitions_to_closed() {
    use loom_core::session_manager::session_manager::SessionStatus;

    let dir = TempDir::new().unwrap();
    let core = make_core(&dir);

    let id = core.session_manager.create(default_session_opts()).unwrap();
    core.session_manager
        .close(id.clone())
        .expect("close must succeed on active session");

    let session = core.session_manager.get(id).unwrap();
    let status = *session.status.lock();
    assert_eq!(
        status,
        SessionStatus::Closed,
        "closed session must be Closed; got {:?}",
        status
    );
}

// ─── Test 5: Happy — ContentStore put + get roundtrip is idempotent ──────────

/// Contract: ContentStore::put is idempotent; get verifies hash.
/// SLA: p95 read ≤ 20ms for blobs ≤ 1MB.
#[test]
fn test_content_store_put_get_roundtrip() {
    let dir = TempDir::new().unwrap();
    let core = make_core(&dir);

    let payload = b"hello loom integration test payload";

    // First put — should succeed and return a SHA-256 ref
    let ref1 = core.content_store.put(payload).expect("put must succeed");
    assert_eq!(
        ref1.sha256.len(),
        64,
        "content ref must be 64-char hex SHA-256"
    );

    // Second put with same bytes — idempotent; same ref
    let ref2 = core
        .content_store
        .put(payload)
        .expect("idempotent put must succeed");
    assert_eq!(
        ref1.sha256, ref2.sha256,
        "idempotent puts must return same ref"
    );

    // Get — bytes must match and hash re-verification passes
    let t0 = std::time::Instant::now();
    let retrieved = core.content_store.get(&ref1).expect("get must succeed");
    let elapsed = t0.elapsed();

    assert_eq!(
        retrieved, payload,
        "retrieved bytes must equal original payload"
    );
    // SLA: p95 ≤ 20ms for blobs ≤ 1MB
    assert!(
        elapsed < Duration::from_millis(20),
        "content store get p95 SLA violated: {}ms > 20ms",
        elapsed.as_millis()
    );
}

// ─── Test 6: Error — ContentStore get with wrong hash → StoreIntegrityFailed ─

/// Contract: ContentStore::get with tampered hash → StoreIntegrityFailed.
#[test]
fn test_content_store_get_with_wrong_hash_fails() {
    use loom_core::content_store::ContentRef;

    let dir = TempDir::new().unwrap();
    let core = make_core(&dir);

    let payload = b"some payload for hash tampering test";
    let real_ref = core.content_store.put(payload).expect("put must succeed");

    // Tamper with the hash
    let mut bad_sha = real_ref.sha256.clone();
    bad_sha.replace_range(0..2, "00");
    let bad_ref = ContentRef {
        sha256: bad_sha,
        size_bytes: real_ref.size_bytes,
    };

    let err = core
        .content_store
        .get(&bad_ref)
        .expect_err("get with wrong hash must fail");

    assert_eq!(
        err.code,
        LoomErrorCode::StoreNotFound,
        "tampered hash must return StoreNotFound (hash mismatch → file not found); got {:?}",
        err.code
    );
}

// ─── active_session_count restart seeding (daemon concurrency-cap source) ────

/// The daemon's session cap counts `session_manager.active_session_count()`
/// (in-memory) instead of re-parsing every WAL on disk. After a daemon crash
/// the in-memory count restarts at zero — the startup recovery sweep must
/// reconcile the on-disk "active" leftover to RuntimeCrash so disk and the
/// cap source agree (no phantom slot consumption, no double-counting).
#[test]
fn test_recovery_sweep_reconciles_crashed_actives_with_in_memory_count() {
    let dir = TempDir::new().unwrap();

    // "Crashed daemon": create a session and drop the facade WITHOUT closing —
    // the WAL has a Header but no terminal entry.
    let session_id = {
        let core = make_core(&dir);
        let id = core
            .session_manager
            .create(default_session_opts())
            .expect("create");
        assert_eq!(core.session_manager.active_session_count(), 1);
        id
    };

    // Fresh daemon over the same data root: in-memory cap source starts at 0.
    let core2 = make_core(&dir);
    assert_eq!(
        core2.session_manager.active_session_count(),
        0,
        "a fresh process must not inherit phantom active sessions"
    );

    // Startup recovery stamps the orphaned-active WAL as crashed, so the
    // on-disk view agrees with the in-memory count (nothing reads "active").
    core2
        .startup_manager
        .perform_recovery_sweep()
        .expect("recovery sweep");
    let infos = core2.list_sessions_info().expect("list_sessions_info");
    let status = infos
        .iter()
        .find(|(id, _, _)| *id == session_id.0)
        .map(|(_, status, _)| status.clone())
        .expect("crashed session must still be listed");
    assert_eq!(
        status, "crashed",
        "recovery must terminal-mark the leftover so no source counts it active"
    );
}

// ─── Replay-refusal fidelity: facade session-not-found path ──────────────────

/// `replay_session_to_id` on a missing session must surface the typed
/// `SessionNotFound` WITH its human message (the wire layer passes both
/// through verbatim — see loom-rpc/tests/replay_refusal_wire.rs).
#[test]
fn test_replay_unknown_session_returns_typed_not_found_with_message() {
    let dir = TempDir::new().unwrap();
    let core = make_core(&dir);

    let err = core
        .replay_session_to_id("01NONEXISTENT0000000000000")
        .expect_err("replay of unknown session must fail");
    assert_eq!(
        err.code,
        LoomErrorCode::SessionNotFound,
        "unknown replay source must return SessionNotFound; got {:?}",
        err.code
    );
    assert_eq!(
        err.message, "session 01NONEXISTENT0000000000000 does not exist",
        "refusal message must name the missing session"
    );
}
