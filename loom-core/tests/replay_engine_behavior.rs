// Behavior tests for replay-engine.
//
// Coverage:
//   test_replay_produces_bit_equal_receipt_bytes
//   test_replay_100x_produces_identical_receipt_bytes
//   test_replay_aborts_on_missing_non_screenshot_blob
//   test_replay_proceeds_on_missing_screenshot_blob
//   test_replay_installs_tape_driven_determinism
//   test_diff_action_count_delta_positive
//   test_diff_action_count_delta_zero_no_extras
//   test_diff_field_level_diff_on_dom_hash_mismatch
//   test_diff_screenshot_excluded_from_differences_count
//   test_diff_screenshot_in_screenshot_diffs_with_flag
//   test_inspect_at_action_5_returns_entries_0_to_5
//   test_inspect_does_not_mutate_manifest
//   test_replay_throughput_structural_exceeds_5x
//   Validate: test_validate_passes_intact_chain_and_present_blobs
//   Validate: test_validate_fails_on_broken_hash_chain
//   Validate: test_validate_fails_on_missing_blob
//   Tape:     test_tape_persisted_and_loaded

use loom_core::budget_enforcer::{BudgetEnforcer, BudgetLimits, LocalBudgetEnforcer};
use loom_core::content_store::{ContentStore, LocalContentStore};
use loom_core::determinism_harness::{DeterminismHarness, SideEffectTape, TapeFrame};
use loom_core::error::LoomErrorCode;
use loom_core::manifest_writer::{LocalManifestWriter, ManifestEntry, ManifestWriter, SessionId};
use loom_core::observability::Observability;
use loom_core::replay_engine::{DiffOpts, LocalReplayEngine, ReplayEngine, ReplayOpts};
use loom_core::session_manager::{LocalSessionManager, SessionStatus};
use loom_core::vault::{KeychainAccess, LocalVault, Vault};
use ring::digest::{digest, SHA256};
use std::path::PathBuf;
use std::sync::Arc;
use zeroize::Zeroizing;

// ---- minimal Keychain stub for LocalVault construction ----

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

// ---- test harness helpers ----

fn tmp_path() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn make_obs(tmp: &tempfile::TempDir) -> Arc<Observability> {
    Observability::new(tmp.path().join("loom.log"), false)
}

fn make_manifest_writer(
    tmp: &tempfile::TempDir,
    obs: Arc<Observability>,
) -> Arc<LocalManifestWriter> {
    Arc::new(LocalManifestWriter::new(tmp.path().join("sessions"), obs))
}

fn make_harness(seed: u64, mw: Arc<dyn ManifestWriter>) -> Arc<DeterminismHarness> {
    Arc::new(DeterminismHarness::new(seed, mw))
}

fn make_content_store(tmp: &tempfile::TempDir, obs: Arc<Observability>) -> Arc<LocalContentStore> {
    Arc::new(LocalContentStore::new(tmp.path().join("store"), obs))
}

fn make_session_manager(
    tmp: &tempfile::TempDir,
    mw: Arc<dyn ManifestWriter>,
    // Sessions mint their own per-session DeterminismHarness at create()
    // now; the parameter is kept so the ~20 call sites stay untouched.
    _dh: Arc<DeterminismHarness>,
    obs: Arc<Observability>,
) -> Arc<LocalSessionManager> {
    let cs: Arc<dyn ContentStore> = Arc::new(LocalContentStore::new(
        tmp.path().join("store"),
        obs.clone(),
    ));
    let kc: Arc<dyn KeychainAccess> = Arc::new(StubKc);
    let v: Arc<dyn Vault> = Arc::new(LocalVault::new(kc, mw.clone(), obs.clone()));
    let be: Arc<dyn BudgetEnforcer> = Arc::new(LocalBudgetEnforcer::new(obs));
    LocalSessionManager::new(
        cs,
        mw,
        v,
        be,
        Observability::new(PathBuf::from("/dev/null"), false),
        0,
        tmp.path().join("sessions"),
    )
}

fn make_engine(
    tmp: &tempfile::TempDir,
    content_store: Arc<dyn ContentStore>,
    mw: Arc<dyn ManifestWriter>,
    dh: Arc<DeterminismHarness>,
    sm: Arc<LocalSessionManager>,
) -> LocalReplayEngine {
    LocalReplayEngine::new(
        content_store,
        mw,
        dh,
        Observability::new(tmp.path().join("replay.log"), false),
        sm,
        tmp.path().join("sessions"),
    )
}

/// Build a minimal recorded session: write Header + N ActionReceipt entries + SessionTerminal.
/// Returns session_id and the receipt bytes used.
fn build_recorded_session(
    mw: &dyn ManifestWriter,
    sessions_root: &std::path::Path,
    n_actions: u64,
    receipt_payload: &[u8],
) -> (SessionId, Vec<Vec<u8>>) {
    let id = SessionId(format!("01TEST{:020}", n_actions));
    std::fs::create_dir_all(sessions_root.join(&id.0)).unwrap();
    mw.open_manifest(id.clone(), None).unwrap();

    let mut receipts = Vec::new();
    for i in 0..n_actions {
        let receipt_json = serde_json::json!({
            "action_id": i,
            "dom_after_hash": sha256_hex(receipt_payload),
            "network_hash": sha256_hex(b"net"),
            "console_lines": i,
        });
        let receipt_bytes = serde_jcs::to_string(&receipt_json).unwrap().into_bytes();
        mw.append(
            id.clone(),
            ManifestEntry::ActionReceipt {
                action_id: i,
                emitted_at_ms: 1_000_000 + i * 100,
                receipt_canonical_bytes: receipt_bytes.clone(),
                prev_hash: String::new(),
            },
        )
        .unwrap();
        receipts.push(receipt_bytes);
    }

    mw.append(
        id.clone(),
        ManifestEntry::SessionTerminal {
            action_id: n_actions,
            emitted_at_ms: 1_000_000 + n_actions * 100,
            reason: "close".to_string(),
            prev_hash: String::new(),
        },
    )
    .unwrap();

    (id, receipts)
}

fn sha256_hex(input: &[u8]) -> String {
    let d = digest(&SHA256, input);
    d.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Extract (action_id, emitted_at_ms, receipt_canonical_bytes) tuples from WAL.
/// These are the fields that replay() copies exactly.
fn extract_action_receipts(
    sessions_root: &std::path::Path,
    id: &SessionId,
) -> Vec<(u64, u64, Vec<u8>)> {
    let content = std::fs::read_to_string(sessions_root.join(&id.0).join("manifest.wal")).unwrap();
    let mut out = Vec::new();
    for line in content.lines() {
        if let Ok(ManifestEntry::ActionReceipt {
            action_id,
            emitted_at_ms,
            receipt_canonical_bytes,
            ..
        }) = serde_json::from_str::<ManifestEntry>(line)
        {
            out.push((action_id, emitted_at_ms, receipt_canonical_bytes));
        }
    }
    out
}

// ---- bit-equal receipt bytes ----

#[test]
fn test_replay_produces_bit_equal_receipt_bytes() {
    let tmp = tmp_path();
    let obs = make_obs(&tmp);
    let sessions_root = tmp.path().join("sessions");
    let mw = make_manifest_writer(&tmp, obs.clone());
    let cs = make_content_store(&tmp, obs.clone());
    let dh = make_harness(42, mw.clone() as Arc<dyn ManifestWriter>);
    let sm = make_session_manager(
        &tmp,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        obs.clone(),
    );
    let engine = make_engine(
        &tmp,
        cs.clone() as Arc<dyn ContentStore>,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        sm.clone(),
    );

    let (source_id, _) = build_recorded_session(
        mw.as_ref() as &dyn ManifestWriter,
        &sessions_root,
        5,
        b"payload",
    );

    let replay_id = engine
        .replay(source_id.clone(), ReplayOpts::default())
        .expect("replay should succeed");

    // receipt_canonical_bytes must be byte-for-byte identical.
    // (emitted_at_ms is also copied from source to preserve hash-chain equality)
    let source_receipts = extract_action_receipts(&sessions_root, &source_id);
    let replay_receipts = extract_action_receipts(&sessions_root, &replay_id);
    assert_eq!(
        source_receipts.len(),
        replay_receipts.len(),
        "same number of actions"
    );
    for ((sa, se, sb), (ra, re, rb)) in source_receipts.iter().zip(replay_receipts.iter()) {
        assert_eq!(sa, ra, "action_id order preserved");
        assert_eq!(se, re, "emitted_at_ms copied from source");
        assert_eq!(
            sb, rb,
            "receipt bytes for action {sa} must be byte-identical"
        );
    }
}

// 100 replays all produce identical receipt bytes
#[test]
fn test_replay_100x_produces_identical_receipt_bytes() {
    let tmp = tmp_path();
    let obs = make_obs(&tmp);
    let sessions_root = tmp.path().join("sessions");
    let mw = make_manifest_writer(&tmp, obs.clone());
    let cs = make_content_store(&tmp, obs.clone());
    let dh = make_harness(42, mw.clone() as Arc<dyn ManifestWriter>);
    let sm = make_session_manager(
        &tmp,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        obs.clone(),
    );
    let engine = make_engine(
        &tmp,
        cs.clone() as Arc<dyn ContentStore>,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        sm.clone(),
    );

    let (source_id, _) = build_recorded_session(
        mw.as_ref() as &dyn ManifestWriter,
        &sessions_root,
        3,
        b"deterministic",
    );
    let source_receipts = extract_action_receipts(&sessions_root, &source_id);

    for _ in 0..100 {
        let replay_id = engine
            .replay(source_id.clone(), ReplayOpts::default())
            .expect("replay should succeed");
        let replay_receipts = extract_action_receipts(&sessions_root, &replay_id);
        assert_eq!(
            source_receipts, replay_receipts,
            "each of 100 replays must have byte-identical receipt content"
        );
    }
}

// ---- replay refuses on missing non-screenshot blob ----

#[test]
fn test_replay_aborts_on_missing_non_screenshot_blob_with_correct_error() {
    let tmp = tmp_path();
    let obs = make_obs(&tmp);
    let sessions_root = tmp.path().join("sessions");
    let mw = make_manifest_writer(&tmp, obs.clone());
    // Use a real but EMPTY content store — all blob gets will return StoreNotFound.
    let cs = make_content_store(&tmp, obs.clone());
    let dh = make_harness(42, mw.clone() as Arc<dyn ManifestWriter>);
    let sm = make_session_manager(
        &tmp,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        obs.clone(),
    );
    let engine = make_engine(
        &tmp,
        cs.clone() as Arc<dyn ContentStore>,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        sm.clone(),
    );

    let id = SessionId("01TESTMISSBLOB0000000".to_string());
    std::fs::create_dir_all(sessions_root.join(&id.0)).unwrap();
    mw.open_manifest(id.clone(), None).unwrap();

    let receipt_json = serde_json::json!({
        "action_id": 0,
        "dom_after_hash": "a".repeat(64),
        "content_refs": [{"sha256": "a".repeat(64), "size_bytes": 100, "kind": "dom"}]
    });
    let receipt_bytes = serde_jcs::to_string(&receipt_json).unwrap().into_bytes();
    mw.append(
        id.clone(),
        ManifestEntry::ActionReceipt {
            action_id: 0,
            emitted_at_ms: 1_000_000,
            receipt_canonical_bytes: receipt_bytes,
            prev_hash: String::new(),
        },
    )
    .unwrap();
    mw.append(
        id.clone(),
        ManifestEntry::SessionTerminal {
            action_id: 1,
            emitted_at_ms: 1_000_100,
            reason: "close".to_string(),
            prev_hash: String::new(),
        },
    )
    .unwrap();

    let result = engine.replay(id, ReplayOpts::default());
    assert!(
        result.is_err(),
        "replay must abort when non-screenshot blob is missing"
    );
    let err = result.unwrap_err();
    assert_eq!(
        err.code,
        LoomErrorCode::ReplayMissingBlob,
        "error code must be ReplayMissingBlob, got {:?}",
        err.code
    );
    // Refusal-fidelity audit: the message names the missing blob + kind so
    // the wire layer can pass it through verbatim.
    assert!(
        err.message.contains("pre-flight: missing blob"),
        "missing-blob refusal must carry the pre-flight explanation; got: {}",
        err.message
    );
}

// Screenshot blobs missing → replay proceeds (not abort)
#[test]
fn test_replay_proceeds_on_missing_screenshot_blob() {
    let tmp = tmp_path();
    let obs = make_obs(&tmp);
    let sessions_root = tmp.path().join("sessions");
    let mw = make_manifest_writer(&tmp, obs.clone());
    let cs = make_content_store(&tmp, obs.clone()); // empty CAS
    let dh = make_harness(42, mw.clone() as Arc<dyn ManifestWriter>);
    let sm = make_session_manager(
        &tmp,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        obs.clone(),
    );
    let engine = make_engine(
        &tmp,
        cs.clone() as Arc<dyn ContentStore>,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        sm.clone(),
    );

    let id = SessionId("01TESTSCREENSHOT00000".to_string());
    std::fs::create_dir_all(sessions_root.join(&id.0)).unwrap();
    mw.open_manifest(id.clone(), None).unwrap();

    let receipt_json = serde_json::json!({
        "action_id": 0,
        "screenshot_hash": "b".repeat(64),
        "content_refs": [{"sha256": "b".repeat(64), "size_bytes": 200, "kind": "screenshot"}]
    });
    let receipt_bytes = serde_jcs::to_string(&receipt_json).unwrap().into_bytes();
    mw.append(
        id.clone(),
        ManifestEntry::ActionReceipt {
            action_id: 0,
            emitted_at_ms: 1_000_000,
            receipt_canonical_bytes: receipt_bytes,
            prev_hash: String::new(),
        },
    )
    .unwrap();
    mw.append(
        id.clone(),
        ManifestEntry::SessionTerminal {
            action_id: 1,
            emitted_at_ms: 1_000_100,
            reason: "close".to_string(),
            prev_hash: String::new(),
        },
    )
    .unwrap();

    let result = engine.replay(id, ReplayOpts::default());
    assert!(
        result.is_ok(),
        "replay must NOT abort for missing screenshot blob: {:?}",
        result.err()
    );
}

// ---- tape-driven determinism installed ----

#[test]
fn test_replay_installs_tape_driven_determinism() {
    let tmp = tmp_path();
    let obs = make_obs(&tmp);
    let sessions_root = tmp.path().join("sessions");
    let mw = make_manifest_writer(&tmp, obs.clone());
    let cs = make_content_store(&tmp, obs.clone());
    let dh = make_harness(42, mw.clone() as Arc<dyn ManifestWriter>);
    let sm = make_session_manager(
        &tmp,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        obs.clone(),
    );

    // Write a tape.jsonl for the source session containing a clock frame
    let source_id = SessionId("01TESTTAPEREP0000000".to_string());
    std::fs::create_dir_all(sessions_root.join(&source_id.0)).unwrap();
    let tape_path = sessions_root.join(&source_id.0).join("tape.jsonl");
    let tape_line = serde_jcs::to_string(&TapeFrame::ClockRead { observed_ns: 9876 }).unwrap();
    std::fs::write(&tape_path, format!("{tape_line}\n")).unwrap();

    mw.open_manifest(source_id.clone(), None).unwrap();
    let receipt_bytes = serde_jcs::to_string(
        &serde_json::json!({"action_id": 0, "dom_after_hash": "c".repeat(64)}),
    )
    .unwrap()
    .into_bytes();
    mw.append(
        source_id.clone(),
        ManifestEntry::ActionReceipt {
            action_id: 0,
            emitted_at_ms: 1_000_000,
            receipt_canonical_bytes: receipt_bytes,
            prev_hash: String::new(),
        },
    )
    .unwrap();
    mw.append(
        source_id.clone(),
        ManifestEntry::SessionTerminal {
            action_id: 1,
            emitted_at_ms: 1_000_100,
            reason: "close".to_string(),
            prev_hash: String::new(),
        },
    )
    .unwrap();

    let engine = make_engine(
        &tmp,
        cs.clone() as Arc<dyn ContentStore>,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        sm.clone(),
    );

    // Replay should succeed — tape was loaded and install_replay_mode called
    let result = engine.replay(source_id, ReplayOpts::default());
    assert!(
        result.is_ok(),
        "replay with tape should succeed: {:?}",
        result.err()
    );
}

// ---- diff action count delta ----

#[test]
fn test_diff_action_count_delta_positive() {
    let tmp = tmp_path();
    let obs = make_obs(&tmp);
    let sessions_root = tmp.path().join("sessions");
    let mw = make_manifest_writer(&tmp, obs.clone());
    let cs = make_content_store(&tmp, obs.clone());
    let dh = make_harness(42, mw.clone() as Arc<dyn ManifestWriter>);
    let sm = make_session_manager(
        &tmp,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        obs.clone(),
    );
    let engine = make_engine(
        &tmp,
        cs.clone() as Arc<dyn ContentStore>,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        sm.clone(),
    );

    let (id_a, _) = build_recorded_session(
        mw.as_ref() as &dyn ManifestWriter,
        &sessions_root,
        3,
        b"payload",
    );
    let (id_b, _) = build_recorded_session(
        mw.as_ref() as &dyn ManifestWriter,
        &sessions_root,
        4,
        b"payload",
    );

    let report = engine
        .diff(
            id_a.clone(),
            id_b.clone(),
            DiffOpts {
                exclude_screenshots: true,
                include_audit_entries: false,
            },
        )
        .expect("diff should succeed");

    assert_eq!(
        report.action_count_delta, 1,
        "B has one more action: delta should be +1"
    );
    assert_eq!(report.a.0, id_a.0);
    assert_eq!(report.b.0, id_b.0);
}

#[test]
fn test_diff_action_count_delta_zero_no_extras() {
    let tmp = tmp_path();
    let obs = make_obs(&tmp);
    let sessions_root = tmp.path().join("sessions");
    let mw = make_manifest_writer(&tmp, obs.clone());
    let cs = make_content_store(&tmp, obs.clone());
    let dh = make_harness(42, mw.clone() as Arc<dyn ManifestWriter>);
    let sm = make_session_manager(
        &tmp,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        obs.clone(),
    );
    let engine = make_engine(
        &tmp,
        cs.clone() as Arc<dyn ContentStore>,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        sm.clone(),
    );

    let (id_a, _) = build_recorded_session(
        mw.as_ref() as &dyn ManifestWriter,
        &sessions_root,
        3,
        b"payload",
    );
    let (id_b, _) = build_recorded_session(
        mw.as_ref() as &dyn ManifestWriter,
        &sessions_root,
        3,
        b"payload",
    );

    let report = engine
        .diff(
            id_a,
            id_b,
            DiffOpts {
                exclude_screenshots: true,
                include_audit_entries: false,
            },
        )
        .expect("diff should succeed");

    assert_eq!(
        report.action_count_delta, 0,
        "identical action counts → delta 0"
    );
    assert!(
        report.field_diffs.is_empty(),
        "identical receipts → no field diffs"
    );
}

// ---- per-receipt field diff ----

#[test]
fn test_diff_field_level_diff_on_dom_hash_mismatch() {
    let tmp = tmp_path();
    let obs = make_obs(&tmp);
    let sessions_root = tmp.path().join("sessions");
    let mw = make_manifest_writer(&tmp, obs.clone());
    let cs = make_content_store(&tmp, obs.clone());
    let dh = make_harness(42, mw.clone() as Arc<dyn ManifestWriter>);
    let sm = make_session_manager(
        &tmp,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        obs.clone(),
    );
    let engine = make_engine(
        &tmp,
        cs.clone() as Arc<dyn ContentStore>,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        sm.clone(),
    );

    // A: action 0 receipt has dom_after_hash of "version-a"
    let (id_a, _) = build_recorded_session(
        mw.as_ref() as &dyn ManifestWriter,
        &sessions_root,
        1,
        b"version-a",
    );
    // B: action 0 receipt has dom_after_hash of "version-b" (different hash)
    let (id_b, _) = build_recorded_session(
        mw.as_ref() as &dyn ManifestWriter,
        &sessions_root,
        1,
        b"version-b",
    );

    let report = engine
        .diff(
            id_a,
            id_b,
            DiffOpts {
                exclude_screenshots: true,
                include_audit_entries: false,
            },
        )
        .expect("diff should succeed");

    assert!(
        !report.field_diffs.is_empty(),
        "different dom_after_hash should produce field diff"
    );
    let diff = &report.field_diffs[0];
    assert_eq!(diff.action_id, 0);
    assert!(
        diff.field_path.contains("dom_after_hash"),
        "field_path should reference dom_after_hash, got: {}",
        diff.field_path
    );
}

// ---- screenshots excluded from differences ----

#[test]
fn test_diff_screenshot_excluded_from_differences_count() {
    let tmp = tmp_path();
    let obs = make_obs(&tmp);
    let sessions_root = tmp.path().join("sessions");
    let mw = make_manifest_writer(&tmp, obs.clone());
    let cs = make_content_store(&tmp, obs.clone());
    let dh = make_harness(42, mw.clone() as Arc<dyn ManifestWriter>);
    let sm = make_session_manager(
        &tmp,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        obs.clone(),
    );
    let engine = make_engine(
        &tmp,
        cs.clone() as Arc<dyn ContentStore>,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        sm.clone(),
    );

    // Session A: screenshot_hash = "eee..."
    let id_a = SessionId("01TESTSCRDIFFA000000".to_string());
    std::fs::create_dir_all(sessions_root.join(&id_a.0)).unwrap();
    mw.open_manifest(id_a.clone(), None).unwrap();
    let receipt_a = serde_jcs::to_string(&serde_json::json!({
        "action_id": 0, "dom_after_hash": "d".repeat(64), "screenshot_hash": "e".repeat(64)
    }))
    .unwrap()
    .into_bytes();
    mw.append(
        id_a.clone(),
        ManifestEntry::ActionReceipt {
            action_id: 0,
            emitted_at_ms: 1_000,
            receipt_canonical_bytes: receipt_a,
            prev_hash: String::new(),
        },
    )
    .unwrap();
    mw.append(
        id_a.clone(),
        ManifestEntry::SessionTerminal {
            action_id: 1,
            emitted_at_ms: 1_100,
            reason: "close".to_string(),
            prev_hash: String::new(),
        },
    )
    .unwrap();

    // Session B: same dom_after_hash, different screenshot_hash
    let id_b = SessionId("01TESTSCRDIFFB000000".to_string());
    std::fs::create_dir_all(sessions_root.join(&id_b.0)).unwrap();
    mw.open_manifest(id_b.clone(), None).unwrap();
    let receipt_b = serde_jcs::to_string(&serde_json::json!({
        "action_id": 0, "dom_after_hash": "d".repeat(64), "screenshot_hash": "f".repeat(64)
    }))
    .unwrap()
    .into_bytes();
    mw.append(
        id_b.clone(),
        ManifestEntry::ActionReceipt {
            action_id: 0,
            emitted_at_ms: 1_000,
            receipt_canonical_bytes: receipt_b,
            prev_hash: String::new(),
        },
    )
    .unwrap();
    mw.append(
        id_b.clone(),
        ManifestEntry::SessionTerminal {
            action_id: 1,
            emitted_at_ms: 1_100,
            reason: "close".to_string(),
            prev_hash: String::new(),
        },
    )
    .unwrap();

    let report = engine
        .diff(
            id_a,
            id_b,
            DiffOpts {
                exclude_screenshots: true,
                include_audit_entries: false,
            },
        )
        .expect("diff should succeed");

    assert!(
        report.field_diffs.is_empty(),
        "screenshot-only diff → field_diffs must be empty (differences=0)"
    );
    assert_eq!(
        report.screenshot_diffs.len(),
        1,
        "screenshot hash mismatch should be in screenshot_diffs"
    );
}

#[test]
fn test_diff_screenshot_in_screenshot_diffs_with_flag() {
    // When exclude_screenshots=false, screenshot diffs appear in screenshot_diffs[] only,
    // NOT in field_diffs[].
    let tmp = tmp_path();
    let obs = make_obs(&tmp);
    let sessions_root = tmp.path().join("sessions");
    let mw = make_manifest_writer(&tmp, obs.clone());
    let cs = make_content_store(&tmp, obs.clone());
    let dh = make_harness(42, mw.clone() as Arc<dyn ManifestWriter>);
    let sm = make_session_manager(
        &tmp,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        obs.clone(),
    );
    let engine = make_engine(
        &tmp,
        cs.clone() as Arc<dyn ContentStore>,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        sm.clone(),
    );

    let id_a = SessionId("01TESTSCRINCLUDA0000".to_string());
    std::fs::create_dir_all(sessions_root.join(&id_a.0)).unwrap();
    mw.open_manifest(id_a.clone(), None).unwrap();
    let ra = serde_jcs::to_string(&serde_json::json!({"action_id":0,"dom_after_hash":"g".repeat(64),"screenshot_hash":"h".repeat(64)}))
        .unwrap()
        .into_bytes();
    mw.append(
        id_a.clone(),
        ManifestEntry::ActionReceipt {
            action_id: 0,
            emitted_at_ms: 1_000,
            receipt_canonical_bytes: ra,
            prev_hash: String::new(),
        },
    )
    .unwrap();
    mw.append(
        id_a.clone(),
        ManifestEntry::SessionTerminal {
            action_id: 1,
            emitted_at_ms: 1_100,
            reason: "close".to_string(),
            prev_hash: String::new(),
        },
    )
    .unwrap();

    let id_b = SessionId("01TESTSCRINCLUB00000".to_string());
    std::fs::create_dir_all(sessions_root.join(&id_b.0)).unwrap();
    mw.open_manifest(id_b.clone(), None).unwrap();
    let rb = serde_jcs::to_string(&serde_json::json!({"action_id":0,"dom_after_hash":"g".repeat(64),"screenshot_hash":"i".repeat(64)}))
        .unwrap()
        .into_bytes();
    mw.append(
        id_b.clone(),
        ManifestEntry::ActionReceipt {
            action_id: 0,
            emitted_at_ms: 1_000,
            receipt_canonical_bytes: rb,
            prev_hash: String::new(),
        },
    )
    .unwrap();
    mw.append(
        id_b.clone(),
        ManifestEntry::SessionTerminal {
            action_id: 1,
            emitted_at_ms: 1_100,
            reason: "close".to_string(),
            prev_hash: String::new(),
        },
    )
    .unwrap();

    // exclude_screenshots: false → screenshot diffs go in screenshot_diffs[], never field_diffs[]
    let report = engine
        .diff(
            id_a,
            id_b,
            DiffOpts {
                exclude_screenshots: false,
                include_audit_entries: false,
            },
        )
        .expect("diff ok");
    assert!(
        report.field_diffs.is_empty(),
        "screenshot diffs must NOT be in field_diffs"
    );
    assert!(
        !report.screenshot_diffs.is_empty(),
        "screenshot diffs should be in screenshot_diffs"
    );
    assert_eq!(report.action_count_delta, 0);
}

// ---- time-travel inspect ----

#[test]
fn test_inspect_at_action_5_returns_entries_0_to_5() {
    let tmp = tmp_path();
    let obs = make_obs(&tmp);
    let sessions_root = tmp.path().join("sessions");
    let mw = make_manifest_writer(&tmp, obs.clone());
    let cs = make_content_store(&tmp, obs.clone());
    let dh = make_harness(42, mw.clone() as Arc<dyn ManifestWriter>);
    let sm = make_session_manager(
        &tmp,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        obs.clone(),
    );
    let engine = make_engine(
        &tmp,
        cs.clone() as Arc<dyn ContentStore>,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        sm.clone(),
    );

    let (id, _) = build_recorded_session(
        mw.as_ref() as &dyn ManifestWriter,
        &sessions_root,
        10,
        b"data",
    );

    let snap = engine
        .inspect(id.clone(), Some(5))
        .expect("inspect should succeed");

    let entries = snap["entries"]
        .as_array()
        .expect("entries must be an array");
    assert_eq!(entries.len(), 6, "at_action=5 → entries 0-5 = 6 entries");
    let last_id = entries.last().unwrap()["action_id"].as_u64().unwrap();
    assert_eq!(last_id, 5, "last entry must be action_id=5");
}

#[test]
fn test_inspect_does_not_mutate_manifest() {
    let tmp = tmp_path();
    let obs = make_obs(&tmp);
    let sessions_root = tmp.path().join("sessions");
    let mw = make_manifest_writer(&tmp, obs.clone());
    let cs = make_content_store(&tmp, obs.clone());
    let dh = make_harness(42, mw.clone() as Arc<dyn ManifestWriter>);
    let sm = make_session_manager(
        &tmp,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        obs.clone(),
    );
    let engine = make_engine(
        &tmp,
        cs.clone() as Arc<dyn ContentStore>,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        sm.clone(),
    );

    let (id, _) = build_recorded_session(
        mw.as_ref() as &dyn ManifestWriter,
        &sessions_root,
        5,
        b"data",
    );

    let wal_before = std::fs::read(sessions_root.join(&id.0).join("manifest.wal")).unwrap();

    engine
        .inspect(id.clone(), Some(3))
        .expect("inspect should succeed");
    engine
        .inspect(id.clone(), Some(1))
        .expect("second inspect should succeed");

    let wal_after = std::fs::read(sessions_root.join(&id.0).join("manifest.wal")).unwrap();
    assert_eq!(
        wal_before, wal_after,
        "inspect must not mutate the manifest WAL"
    );
}

// ---- replay speed >= 5x real-time ----

#[test]
fn test_replay_throughput_structural_exceeds_5x() {
    // Structural replay is disk I/O only (no WASM execution).
    // Simulate a 60s real-time session with 600 actions (100ms each).
    // Replay should complete in <<12s (60/5). We assert it completes in <5s.
    let tmp = tmp_path();
    let obs = make_obs(&tmp);
    let sessions_root = tmp.path().join("sessions");
    let mw = make_manifest_writer(&tmp, obs.clone());
    let cs = make_content_store(&tmp, obs.clone());
    let dh = make_harness(42, mw.clone() as Arc<dyn ManifestWriter>);
    let sm = make_session_manager(
        &tmp,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        obs.clone(),
    );
    let engine = make_engine(
        &tmp,
        cs.clone() as Arc<dyn ContentStore>,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        sm.clone(),
    );

    let (id, _) = build_recorded_session(
        mw.as_ref() as &dyn ManifestWriter,
        &sessions_root,
        600,
        b"perf-payload",
    );

    let start = std::time::Instant::now();
    engine
        .replay(id, ReplayOpts::default())
        .expect("replay should succeed");
    let elapsed = start.elapsed();

    assert!(
        elapsed.as_secs() < 5,
        "structural replay of 600 actions must complete in <5s, took {:?}",
        elapsed
    );
}

// ---- Validate tests ----

#[test]
fn test_validate_passes_intact_chain_and_present_blobs() {
    let tmp = tmp_path();
    let obs = make_obs(&tmp);
    let sessions_root = tmp.path().join("sessions");
    let mw = make_manifest_writer(&tmp, obs.clone());
    let cs = make_content_store(&tmp, obs.clone());
    let dh = make_harness(42, mw.clone() as Arc<dyn ManifestWriter>);
    let sm = make_session_manager(
        &tmp,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        obs.clone(),
    );
    let engine = make_engine(
        &tmp,
        cs.clone() as Arc<dyn ContentStore>,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        sm.clone(),
    );

    // Store a blob in CAS
    let blob = b"important content";
    let cr = cs.put(blob).unwrap();

    let id = SessionId("01TESTVALIDOKK000000".to_string());
    std::fs::create_dir_all(sessions_root.join(&id.0)).unwrap();
    mw.open_manifest(id.clone(), None).unwrap();
    let receipt = serde_jcs::to_string(&serde_json::json!({
        "action_id": 0,
        "dom_after_hash": cr.sha256,
        "content_refs": [{"sha256": cr.sha256, "size_bytes": cr.size_bytes, "kind": "dom"}]
    }))
    .unwrap()
    .into_bytes();
    mw.append(
        id.clone(),
        ManifestEntry::ActionReceipt {
            action_id: 0,
            emitted_at_ms: 1_000,
            receipt_canonical_bytes: receipt,
            prev_hash: String::new(),
        },
    )
    .unwrap();
    mw.append(
        id.clone(),
        ManifestEntry::SessionTerminal {
            action_id: 1,
            emitted_at_ms: 1_100,
            reason: "close".to_string(),
            prev_hash: String::new(),
        },
    )
    .unwrap();

    let result = engine.validate(id).expect("validate should succeed");
    assert!(
        result.passed,
        "intact chain + present blobs → validate passes"
    );
    assert!(
        result.reasons.is_empty(),
        "no reasons on pass: {:?}",
        result.reasons
    );
}

#[test]
fn test_validate_fails_on_broken_hash_chain() {
    let tmp = tmp_path();
    let obs = make_obs(&tmp);
    let sessions_root = tmp.path().join("sessions");
    let mw = make_manifest_writer(&tmp, obs.clone());
    let cs = make_content_store(&tmp, obs.clone());
    let dh = make_harness(42, mw.clone() as Arc<dyn ManifestWriter>);
    let sm = make_session_manager(
        &tmp,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        obs.clone(),
    );
    let engine = make_engine(
        &tmp,
        cs.clone() as Arc<dyn ContentStore>,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        sm.clone(),
    );

    let id = SessionId("01TESTBROKENHASH0000".to_string());
    std::fs::create_dir_all(sessions_root.join(&id.0)).unwrap();
    mw.open_manifest(id.clone(), None).unwrap();
    let receipt = serde_jcs::to_string(&serde_json::json!({"action_id": 0}))
        .unwrap()
        .into_bytes();
    mw.append(
        id.clone(),
        ManifestEntry::ActionReceipt {
            action_id: 0,
            emitted_at_ms: 1_000,
            receipt_canonical_bytes: receipt,
            prev_hash: String::new(),
        },
    )
    .unwrap();
    mw.append(
        id.clone(),
        ManifestEntry::SessionTerminal {
            action_id: 1,
            emitted_at_ms: 1_100,
            reason: "close".to_string(),
            prev_hash: String::new(),
        },
    )
    .unwrap();

    // Corrupt the WAL by appending a line with a bad prev_hash
    let wal_path = sessions_root.join(&id.0).join("manifest.wal");
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&wal_path)
        .unwrap();
    writeln!(f, r#"{{"kind":"action_receipt","prev_hash":"bad_hash","action_id":99,"emitted_at_ms":9,"receipt_canonical_bytes":[]}}"#).unwrap();

    let result = engine.validate(id).expect("validate call should not panic");
    assert!(!result.passed, "broken hash chain → validate must fail");
    assert!(
        !result.reasons.is_empty(),
        "must provide reason for failure"
    );
}

#[test]
fn test_validate_fails_on_missing_blob() {
    let tmp = tmp_path();
    let obs = make_obs(&tmp);
    let sessions_root = tmp.path().join("sessions");
    let mw = make_manifest_writer(&tmp, obs.clone());
    let cs = make_content_store(&tmp, obs.clone()); // empty CAS
    let dh = make_harness(42, mw.clone() as Arc<dyn ManifestWriter>);
    let sm = make_session_manager(
        &tmp,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        obs.clone(),
    );
    let engine = make_engine(
        &tmp,
        cs.clone() as Arc<dyn ContentStore>,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        sm.clone(),
    );

    let id = SessionId("01TESTVALIDMISSB0000".to_string());
    std::fs::create_dir_all(sessions_root.join(&id.0)).unwrap();
    mw.open_manifest(id.clone(), None).unwrap();
    let receipt = serde_jcs::to_string(&serde_json::json!({
        "action_id": 0,
        "dom_after_hash": "j".repeat(64),
        "content_refs": [{"sha256": "j".repeat(64), "size_bytes": 100, "kind": "dom"}]
    }))
    .unwrap()
    .into_bytes();
    mw.append(
        id.clone(),
        ManifestEntry::ActionReceipt {
            action_id: 0,
            emitted_at_ms: 1_000,
            receipt_canonical_bytes: receipt,
            prev_hash: String::new(),
        },
    )
    .unwrap();
    mw.append(
        id.clone(),
        ManifestEntry::SessionTerminal {
            action_id: 1,
            emitted_at_ms: 1_100,
            reason: "close".to_string(),
            prev_hash: String::new(),
        },
    )
    .unwrap();

    let result = engine.validate(id).expect("validate call should not panic");
    assert!(!result.passed, "missing blob → validate must fail");
    let reason_contains_blob = result
        .reasons
        .iter()
        .any(|r| r.contains("missing") || r.contains("blob") || r.contains("StoreNotFound"));
    assert!(
        reason_contains_blob,
        "reason must mention missing blob, got: {:?}",
        result.reasons
    );
}

// (audit 2026-06-10, "Blob-presence validation and replay pre-flight inspect a
// 'content_refs' field that no production receipt contains"):
// `collect_content_refs` now walks the real `ReceiptPayload` blob-ref fields
// (`dom_after_blob_ref`, `dom_before_blob_ref`, `return_value_blob_ref`,
// `screenshot_*_blob_ref`, `network_events[].response_body_ref`), so
// `validate()` and the `ReplayMissingBlob` pre-flight detect missing CAS blobs
// for real recordings — not just the phantom `content_refs` array. This test
// pins that a missing `dom_after_blob_ref` blob fails validation. FIXED.
#[test]
fn validate_must_fail_on_missing_production_shape_blob_ref() {
    let tmp = tmp_path();
    let obs = make_obs(&tmp);
    let sessions_root = tmp.path().join("sessions");
    let mw = make_manifest_writer(&tmp, obs.clone());
    let cs = make_content_store(&tmp, obs.clone()); // empty CAS — nothing present
    let dh = make_harness(42, mw.clone() as Arc<dyn ManifestWriter>);
    let sm = make_session_manager(
        &tmp,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        obs.clone(),
    );
    let engine = make_engine(
        &tmp,
        cs.clone() as Arc<dyn ContentStore>,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        sm.clone(),
    );

    let id = SessionId("01TESTVALIDPRODREF00".to_string());
    std::fs::create_dir_all(sessions_root.join(&id.0)).unwrap();
    mw.open_manifest(id.clone(), None).unwrap();
    // PRODUCTION receipt shape: named blob-ref fields, NO `content_refs`.
    // This is what `ReceiptMarshaller` / `ReceiptPayload` actually emit.
    let receipt = serde_jcs::to_string(&serde_json::json!({
        "action_id": 0,
        "dom_after_hash": "j".repeat(64),
        "dom_after_blob_ref": {"sha256": "j".repeat(64), "size_bytes": 100}
    }))
    .unwrap()
    .into_bytes();
    mw.append(
        id.clone(),
        ManifestEntry::ActionReceipt {
            action_id: 0,
            emitted_at_ms: 1_000,
            receipt_canonical_bytes: receipt,
            prev_hash: String::new(),
        },
    )
    .unwrap();
    mw.append(
        id.clone(),
        ManifestEntry::SessionTerminal {
            action_id: 1,
            emitted_at_ms: 1_100,
            reason: "close".to_string(),
            prev_hash: String::new(),
        },
    )
    .unwrap();

    let result = engine.validate(id).expect("validate call should not panic");
    assert!(
        !result.passed,
        "a receipt whose dom_after_blob_ref points at a blob absent from the \
         CAS must fail validation — production receipts never carry the \
         legacy `content_refs` array this check currently looks for"
    );
}

// ---- Tape persistence ----

#[test]
fn test_tape_persisted_and_loaded() {
    let tmp = tmp_path();
    let sessions_root = tmp.path().join("sessions");
    let session_id = "01TESTTAPEIO00000000";
    std::fs::create_dir_all(sessions_root.join(session_id)).unwrap();

    let obs = Observability::new(tmp.path().join("loom.log"), false);
    let mw: Arc<dyn ManifestWriter> =
        Arc::new(LocalManifestWriter::new(tmp.path().join("sessions"), obs));
    let dh = DeterminismHarness::new(42, mw);
    let mut tw = dh.new_tape_writer();
    tw.record(TapeFrame::ClockRead { observed_ns: 12345 });
    tw.record(TapeFrame::RngDraw {
        value_u64: 0xdeadbeef,
    });
    tw.record(TapeFrame::ClockRead { observed_ns: 99999 });

    // Persist to disk
    tw.persist(&sessions_root, session_id)
        .expect("persist should succeed");

    // Load back
    let loaded =
        SideEffectTape::load_from_file(&sessions_root, session_id).expect("load should succeed");

    assert_eq!(loaded.frames.len(), 3, "should load 3 frames");
    match &loaded.frames[0] {
        TapeFrame::ClockRead { observed_ns } => assert_eq!(*observed_ns, 12345),
        _ => panic!("frame 0 should be ClockRead"),
    }
    match &loaded.frames[1] {
        TapeFrame::RngDraw { value_u64 } => assert_eq!(*value_u64, 0xdeadbeef),
        _ => panic!("frame 1 should be RngDraw"),
    }
}

// ---- started_at_ms is propagated from source to replay ----
//
// Verifies that the replay session's manifest Header carries the source
// session's `started_at_ms` (not `now_ms()` at replay time). This is the
// foundation for hash-chain bit-equality: the chain
// hashes over the canonical Header bytes, so any divergence in the
// Header poisons every subsequent prev_hash.

#[test]
fn test_replay_header_started_at_ms_matches_source() {
    let tmp = tmp_path();
    let obs = make_obs(&tmp);
    let sessions_root = tmp.path().join("sessions");
    let mw = make_manifest_writer(&tmp, obs.clone());
    let cs = make_content_store(&tmp, obs.clone());
    let dh = make_harness(42, mw.clone() as Arc<dyn ManifestWriter>);
    let sm = make_session_manager(
        &tmp,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        obs.clone(),
    );
    let engine = make_engine(
        &tmp,
        cs.clone() as Arc<dyn ContentStore>,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        sm,
    );

    // Build a small recorded session.
    let (source_id, _receipts) =
        build_recorded_session(mw.as_ref(), &sessions_root, 1, b"ac-shcrt-08-payload");

    // Read the source's started_at_ms from its Header.
    let source_started_at_ms = read_header_started_at_ms(&sessions_root, &source_id)
        .expect("source session must have a Header entry");

    // Replay.
    let replay_id = engine
        .replay(source_id.clone(), ReplayOpts::default())
        .expect("replay must succeed");

    // The replay session's Header must carry the source's started_at_ms.
    let replay_started_at_ms = read_header_started_at_ms(&sessions_root, &replay_id)
        .expect("replay session must have a Header entry");

    assert_eq!(
        source_started_at_ms, replay_started_at_ms,
        "replay Header started_at_ms must equal source's \
         (chain bit-equality requires deterministic Header bytes)"
    );

    // The action_receipt's prev_hash MUST also match the source — the
    // chain hashes over the Header's canonical bytes, and with
    // started_at_ms now equal the only Header field that differs is
    // session_id. Confirm at least the receipt content is bit-equal
    // (this is the existing bit-equal-receipt guarantee).
    let source_receipts = extract_action_receipts(&sessions_root, &source_id);
    let replay_receipts = extract_action_receipts(&sessions_root, &replay_id);
    assert_eq!(
        source_receipts, replay_receipts,
        "replay action_receipts must be bit-equal to source"
    );
}

/// Helper: read the `started_at_ms` field from the Header entry (first
/// line of `manifest.wal`).
fn read_header_started_at_ms(sessions_root: &std::path::Path, id: &SessionId) -> Option<u64> {
    let path = sessions_root.join(&id.0).join("manifest.wal");
    let content = std::fs::read_to_string(&path).ok()?;
    let first = content.lines().next()?;
    if let Ok(ManifestEntry::Header { started_at_ms, .. }) =
        serde_json::from_str::<ManifestEntry>(first)
    {
        Some(started_at_ms)
    } else {
        None
    }
}

#[test]
fn two_consecutive_replays_produce_identical_headers() {
    // Replay determinism: replaying the same source twice produces two
    // sessions with identical Header started_at_ms values (both equal
    // to the source). This is the meaningful "deterministic chain"
    // guarantee given that session_id intentionally differs per session.
    let tmp = tmp_path();
    let obs = make_obs(&tmp);
    let sessions_root = tmp.path().join("sessions");
    let mw = make_manifest_writer(&tmp, obs.clone());
    let cs = make_content_store(&tmp, obs.clone());
    let dh = make_harness(42, mw.clone() as Arc<dyn ManifestWriter>);
    let sm = make_session_manager(
        &tmp,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        obs.clone(),
    );
    let engine = make_engine(
        &tmp,
        cs.clone() as Arc<dyn ContentStore>,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        sm,
    );

    let (source_id, _) =
        build_recorded_session(mw.as_ref(), &sessions_root, 1, b"two-replays-payload");
    let source_ts = read_header_started_at_ms(&sessions_root, &source_id).unwrap();

    let r1 = engine
        .replay(source_id.clone(), ReplayOpts::default())
        .unwrap();
    let r2 = engine
        .replay(source_id.clone(), ReplayOpts::default())
        .unwrap();

    let r1_ts = read_header_started_at_ms(&sessions_root, &r1).unwrap();
    let r2_ts = read_header_started_at_ms(&sessions_root, &r2).unwrap();

    assert_eq!(
        source_ts, r1_ts,
        "replay 1 Header started_at_ms must equal source"
    );
    assert_eq!(
        source_ts, r2_ts,
        "replay 2 Header started_at_ms must equal source"
    );
    assert_eq!(
        r1_ts, r2_ts,
        "two consecutive replays must produce identical Header timestamps"
    );
}

// ---- settle-capture (4b): replay refuses a non-deterministic session ----

/// Build a recorded session whose Header records `determinism_enabled = false`
/// (the `--no-determinism` shape) + one ActionReceipt + a terminal. Mirrors
/// `build_recorded_session` but threads the determinism flag into the Header.
fn build_non_deterministic_session(
    mw: &dyn ManifestWriter,
    sessions_root: &std::path::Path,
) -> SessionId {
    let id = SessionId("01TESTNODETERMINISM00000000".to_string());
    std::fs::create_dir_all(sessions_root.join(&id.0)).unwrap();
    // determinism_enabled = false → the replay-refuse marker.
    mw.open_manifest_with_started_at(id.clone(), None, Some(1_000_000), None, None, false)
        .unwrap();
    mw.append(
        id.clone(),
        ManifestEntry::ActionReceipt {
            action_id: 0,
            emitted_at_ms: 1_000_100,
            receipt_canonical_bytes: serde_jcs::to_string(&serde_json::json!({"action_id": 0}))
                .unwrap()
                .into_bytes(),
            prev_hash: String::new(),
        },
    )
    .unwrap();
    mw.append(
        id.clone(),
        ManifestEntry::SessionTerminal {
            action_id: 1,
            emitted_at_ms: 1_000_200,
            reason: "close".to_string(),
            prev_hash: String::new(),
        },
    )
    .unwrap();
    id
}

#[test]
fn replay_refuses_non_deterministic_session() {
    let tmp = tmp_path();
    let obs = make_obs(&tmp);
    let sessions_root = tmp.path().join("sessions");
    let mw = make_manifest_writer(&tmp, obs.clone());
    let cs = make_content_store(&tmp, obs.clone());
    let dh = make_harness(42, mw.clone() as Arc<dyn ManifestWriter>);
    let sm = make_session_manager(
        &tmp,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        obs.clone(),
    );
    let engine = make_engine(
        &tmp,
        cs.clone() as Arc<dyn ContentStore>,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        sm.clone(),
    );

    let source_id =
        build_non_deterministic_session(mw.as_ref() as &dyn ManifestWriter, &sessions_root);

    let err = engine
        .replay(source_id.clone(), ReplayOpts::default())
        .expect_err("replay MUST refuse a --no-determinism session (it can never be replay-equal)");
    assert_eq!(
        err.code,
        loom_core::error::LoomErrorCode::NotReplayable,
        "refusal must be a typed NotReplayable (NOT InvalidArgument, which \
         degrades to schema_violation on the wire), got {err:?}"
    );
    assert!(
        err.message.contains("--no-determinism") && err.message.contains("NOT replayable"),
        "refusal message must carry the full compiled-in explanation; got: {}",
        err.message
    );
    assert!(
        err.message.contains(&source_id.0),
        "refusal message must name the offending session; got: {}",
        err.message
    );
}

// ---- replay-refusal fidelity audit: every refusal path must emit its ----
// ---- intended typed code + human message (no catch-all degradation). ----
// The wire-level counterpart (code + message surviving to the JSON-RPC
// envelope) is pinned by loom-rpc/tests/replay_refusal_wire.rs.

/// Shorthand: full engine stack on a fresh tmp dir.
fn make_refusal_stack(
    tmp: &tempfile::TempDir,
) -> (
    std::path::PathBuf,
    Arc<LocalManifestWriter>,
    LocalReplayEngine,
) {
    let obs = make_obs(tmp);
    let sessions_root = tmp.path().join("sessions");
    let mw = make_manifest_writer(tmp, obs.clone());
    let cs = make_content_store(tmp, obs.clone());
    let dh = make_harness(42, mw.clone() as Arc<dyn ManifestWriter>);
    let sm = make_session_manager(tmp, mw.clone() as Arc<dyn ManifestWriter>, dh.clone(), obs);
    let engine = make_engine(
        tmp,
        cs as Arc<dyn ContentStore>,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh,
        sm,
    );
    (sessions_root, mw, engine)
}

#[test]
fn replay_refuses_crashed_source_with_typed_session_aborted() {
    let tmp = tmp_path();
    let (sessions_root, mw, engine) = make_refusal_stack(&tmp);

    let id = SessionId("01TESTCRASHEDSOURCE000000".to_string());
    std::fs::create_dir_all(sessions_root.join(&id.0)).unwrap();
    mw.open_manifest(id.clone(), None).unwrap();
    mw.append(
        id.clone(),
        ManifestEntry::RuntimeCrash {
            last_completed_action_id: 0,
            emitted_at_ms: 1_000_100,
            prev_hash: String::new(),
        },
    )
    .unwrap();

    let err = engine
        .replay(id.clone(), ReplayOpts::default())
        .expect_err("replay must refuse a crashed source");
    assert_eq!(err.code, LoomErrorCode::SessionAborted);
    assert_eq!(
        err.message,
        format!(
            "session {} crashed mid-flow; replay refuses to reproduce a partial trace",
            id.0
        ),
        "crashed-source refusal must carry its compiled-in explanation"
    );
}

#[test]
fn replay_refuses_aborted_source_with_typed_session_aborted() {
    let tmp = tmp_path();
    let (sessions_root, mw, engine) = make_refusal_stack(&tmp);

    let id = SessionId("01TESTABORTEDSOURCE000000".to_string());
    std::fs::create_dir_all(sessions_root.join(&id.0)).unwrap();
    mw.open_manifest(id.clone(), None).unwrap();
    mw.append(
        id.clone(),
        ManifestEntry::SessionTerminal {
            action_id: 0,
            emitted_at_ms: 1_000_100,
            reason: "user-initiated".to_string(),
            prev_hash: String::new(),
        },
    )
    .unwrap();

    let err = engine
        .replay(id.clone(), ReplayOpts::default())
        .expect_err("replay must refuse an aborted source");
    assert_eq!(err.code, LoomErrorCode::SessionAborted);
    assert_eq!(
        err.message,
        format!(
            "session {} ended via abort (reason=user-initiated); \
             replay refuses to reproduce an abandoned trace",
            id.0
        ),
        "aborted-source refusal must carry the abort reason"
    );
}

#[test]
fn replay_refuses_broken_chain_with_typed_manifest_corrupt() {
    let tmp = tmp_path();
    let (sessions_root, mw, engine) = make_refusal_stack(&tmp);

    let (id, _) = build_recorded_session(mw.as_ref(), &sessions_root, 1, b"chain-tamper-payload");

    // Tamper: append a line whose prev_hash cannot match.
    let wal_path = sessions_root.join(&id.0).join("manifest.wal");
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&wal_path)
        .unwrap();
    writeln!(f, r#"{{"kind":"action_receipt","prev_hash":"bad_hash","action_id":99,"emitted_at_ms":9,"receipt_canonical_bytes":[]}}"#).unwrap();

    let err = engine
        .replay(id, ReplayOpts::default())
        .expect_err("replay must refuse a broken hash chain");
    assert_eq!(
        err.code,
        LoomErrorCode::ManifestCorrupt,
        "broken chain must surface as ManifestCorrupt (NOT a store/internal catch-all)"
    );
    assert!(
        err.message.contains("hash chain broken at index"),
        "chain refusal must name the break point; got: {}",
        err.message
    );
    assert!(
        err.context.is_some(),
        "chain refusal carries structured context (failed_at_index, hashes)"
    );
}

// === replay Header fidelity: budgets + capture_policy (audit 2026-06-10) ===

/// Read every WAL line's `prev_hash` field as a string ("" for the Header's
/// null). The prev_hash chain seeds from the projected Header bytes, so two
/// manifests with equal vectors have bit-equal chains at every line index.
fn read_prev_hashes(sessions_root: &std::path::Path, id: &SessionId) -> Vec<String> {
    let content = std::fs::read_to_string(sessions_root.join(&id.0).join("manifest.wal")).unwrap();
    content
        .lines()
        .map(|line| {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            v.get("prev_hash")
                .and_then(|p| p.as_str())
                .unwrap_or("")
                .to_string()
        })
        .collect()
}

/// First WAL line (the Header) parsed as JSON.
fn read_header_json(sessions_root: &std::path::Path, id: &SessionId) -> serde_json::Value {
    let content = std::fs::read_to_string(sessions_root.join(&id.0).join("manifest.wal")).unwrap();
    serde_json::from_str(content.lines().next().expect("WAL has a Header line")).unwrap()
}

// Regression: a source recorded with --budget/--capture-policy must replay
// with the SAME Header budgets/capture_policy. Both fields serialize with
// `skip_serializing_if`, and hashable_line() only projects out
// session_id/started_at_ms/emitted_at_ms — so dropping them (the pre-fix
// `limits: None, capture_policy: None`) changed the projected Header hash and
// poisoned every subsequent prev_hash in the replay chain.
#[test]
fn test_replay_header_preserves_budgets_and_capture_policy_chain_bit_equal() {
    let tmp = tmp_path();
    let obs = make_obs(&tmp);
    let sessions_root = tmp.path().join("sessions");
    let mw = make_manifest_writer(&tmp, obs.clone());
    let cs = make_content_store(&tmp, obs.clone());
    let dh = make_harness(42, mw.clone() as Arc<dyn ManifestWriter>);
    let sm = make_session_manager(
        &tmp,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        obs.clone(),
    );
    let engine = make_engine(
        &tmp,
        cs.clone() as Arc<dyn ContentStore>,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        sm.clone(),
    );

    // Source recorded with explicit budgets + capture policy (+ seed, so the
    // replay Header round-trips every skip-if-none field).
    let limits = BudgetLimits {
        session_walltime_ms: 120_000,
        action_walltime_ms: 9_000,
        network_bytes: 1_000_000,
        dom_nodes: 7_000,
        js_heap_bytes: 64 * 1024 * 1024,
    };
    let source_id = SessionId("01TESTHEADERFIDELITY0000AB".to_string());
    std::fs::create_dir_all(sessions_root.join(&source_id.0)).unwrap();
    mw.open_manifest_with_started_at(
        source_id.clone(),
        Some(limits),
        None,
        Some("minimal".to_string()),
        Some(7),
        true,
    )
    .unwrap();
    for i in 0..2u64 {
        mw.append(
            source_id.clone(),
            ManifestEntry::ActionReceipt {
                action_id: i,
                emitted_at_ms: 1_000_000 + i * 100,
                receipt_canonical_bytes: serde_jcs::to_string(&serde_json::json!({"action_id": i}))
                    .unwrap()
                    .into_bytes(),
                prev_hash: String::new(),
            },
        )
        .unwrap();
    }
    mw.append(
        source_id.clone(),
        ManifestEntry::SessionTerminal {
            action_id: 2,
            emitted_at_ms: 1_000_300,
            reason: "close".to_string(),
            prev_hash: String::new(),
        },
    )
    .unwrap();

    let replay_id = engine
        .replay(source_id.clone(), ReplayOpts::default())
        .expect("replay of a budget/capture-policy session should succeed");

    // The replay Header carries the source's recorded budgets + capture_policy.
    let source_header = read_header_json(&sessions_root, &source_id);
    let replay_header = read_header_json(&sessions_root, &replay_id);
    assert_eq!(
        replay_header.get("budgets"),
        source_header.get("budgets"),
        "replay Header must reproduce the source's recorded budgets"
    );
    assert!(
        source_header.get("budgets").is_some(),
        "precondition: source Header actually recorded budgets"
    );
    assert_eq!(
        replay_header.get("capture_policy"),
        source_header.get("capture_policy"),
        "replay Header must reproduce the source's capture_policy"
    );
    assert_eq!(
        source_header.get("capture_policy").and_then(|v| v.as_str()),
        Some("minimal"),
        "precondition: source Header actually recorded capture_policy"
    );

    // Header fidelity modulo the two projected ephemerals: stripping
    // session_id + started_at_ms, the Headers must be IDENTICAL (JCS sorts
    // keys, so Value equality == canonical-byte equality).
    let strip = |mut v: serde_json::Value| {
        let obj = v.as_object_mut().unwrap();
        obj.remove("session_id");
        obj.remove("started_at_ms");
        v
    };
    assert_eq!(
        strip(source_header),
        strip(replay_header),
        "replay Header must match the source Header on every non-ephemeral field"
    );

    // Chain bit-equality: the prev_hash at EVERY line index must match the
    // source's (index 1 is sha256 of the projected Header — the chain seed).
    assert_eq!(
        read_prev_hashes(&sessions_root, &source_id),
        read_prev_hashes(&sessions_root, &replay_id),
        "replay prev_hash chain must be bit-equal to the source chain at every index"
    );

    mw.validate(replay_id)
        .expect("replay manifest hash chain must validate");
}

// === replay closes its session coherently through the SessionManager ===

// Regression (audit 2026-06-10): replay() used to append the
// 'replay_complete' SessionTerminal directly via manifest_writer, leaving the
// in-memory session Active with last_activity_ms pinned to the SOURCE's
// original started_at_ms — instantly idle-reapable, so the reaper appended a
// SECOND SessionTerminal{idle_ttl} over the completed replay manifest.
#[test]
fn test_replay_closes_session_in_fsm_and_reaper_cannot_double_terminal() {
    let tmp = tmp_path();
    let obs = make_obs(&tmp);
    let sessions_root = tmp.path().join("sessions");
    let mw = make_manifest_writer(&tmp, obs.clone());
    let cs = make_content_store(&tmp, obs.clone());
    let dh = make_harness(42, mw.clone() as Arc<dyn ManifestWriter>);
    let sm = make_session_manager(
        &tmp,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        obs.clone(),
    );
    let engine = make_engine(
        &tmp,
        cs.clone() as Arc<dyn ContentStore>,
        mw.clone() as Arc<dyn ManifestWriter>,
        dh.clone(),
        sm.clone(),
    );

    let (source_id, _) = build_recorded_session(
        mw.as_ref() as &dyn ManifestWriter,
        &sessions_root,
        2,
        b"fsm-close",
    );

    let replay_id = engine
        .replay(source_id, ReplayOpts::default())
        .expect("replay should succeed");

    // The in-memory FSM must be terminal, not Active.
    let session = sm
        .get(replay_id.clone())
        .expect("replay session retained in-memory (bounded terminal retention)");
    assert_eq!(
        *session.status.lock(),
        SessionStatus::Closed,
        "replay() must close its session through the SessionManager FSM"
    );

    // Simulate the idle reaper hitting the session with an ancient
    // last_activity clock: the two-phase guard must SPARE it (not Active),
    // never appending a second terminal.
    let far_future = 4_102_444_800_000u64; // 2100-01-01 — any 'now' past the source epoch
    let evicted = sm
        .evict_if_idle(replay_id.clone(), 1, far_future)
        .expect("evict_if_idle on a closed session is not an error for the sweep");
    assert!(
        !evicted,
        "idle reaper must spare the already-closed replay session"
    );

    // Exactly ONE SessionTerminal, with reason 'replay_complete'.
    let content =
        std::fs::read_to_string(sessions_root.join(&replay_id.0).join("manifest.wal")).unwrap();
    let terminals: Vec<serde_json::Value> = content
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["kind"] == "session_terminal")
        .collect();
    assert_eq!(
        terminals.len(),
        1,
        "replay manifest must contain exactly one SessionTerminal"
    );
    assert_eq!(
        terminals[0]["reason"], "replay_complete",
        "the single terminal must be the replay_complete one"
    );

    // Replay-of-replay stays allowed (the 1b abort-guard accepts
    // reason=replay_complete; an idle_ttl double-terminal would refuse it).
    engine
        .replay(replay_id, ReplayOpts::default())
        .expect("replaying a cleanly completed replay must stay allowed");
}

// ---- ValidationResult.replayable: PASS ≠ replayable ----

#[test]
fn validate_reports_no_determinism_session_as_pass_but_not_replayable() {
    let tmp = tmp_path();
    let (sessions_root, mw, engine) = make_refusal_stack(&tmp);

    let id = build_non_deterministic_session(mw.as_ref() as &dyn ManifestWriter, &sessions_root);

    let result = engine
        .validate(id.clone())
        .expect("validate must not error");
    assert!(
        result.passed,
        "a --no-determinism session has an intact chain → integrity PASSes"
    );
    assert!(
        !result.replayable,
        "PASS must not imply replayable for a --no-determinism session"
    );
    let reason = result.not_replayable_reason.expect("reason must be set");
    assert!(
        reason.contains("--no-determinism") && reason.contains(&id.0),
        "reason must be the replay refusal explanation; got: {reason}"
    );
}

#[test]
fn validate_reports_clean_deterministic_session_as_replayable() {
    let tmp = tmp_path();
    let (sessions_root, mw, engine) = make_refusal_stack(&tmp);

    // No content_refs → no blob requirements; clean close terminal.
    let (id, _) = build_recorded_session(mw.as_ref(), &sessions_root, 1, b"replayable-payload");

    let result = engine.validate(id).expect("validate must not error");
    assert!(
        result.passed,
        "clean session must PASS: {:?}",
        result.reasons
    );
    assert!(
        result.replayable,
        "clean deterministic session is replayable"
    );
    assert!(result.not_replayable_reason.is_none());
}

#[test]
fn validate_reports_aborted_session_as_not_replayable() {
    let tmp = tmp_path();
    let (sessions_root, mw, engine) = make_refusal_stack(&tmp);

    let id = SessionId("01TESTVALIDATEABORTED0000".to_string());
    std::fs::create_dir_all(sessions_root.join(&id.0)).unwrap();
    mw.open_manifest(id.clone(), None).unwrap();
    mw.append(
        id.clone(),
        ManifestEntry::SessionTerminal {
            action_id: 0,
            emitted_at_ms: 1_000_100,
            reason: "user-initiated".to_string(),
            prev_hash: String::new(),
        },
    )
    .unwrap();

    let result = engine.validate(id).expect("validate must not error");
    assert!(result.passed, "intact chain → integrity PASSes");
    assert!(
        !result.replayable,
        "aborted source is refused by replay → not replayable"
    );
    assert!(
        result
            .not_replayable_reason
            .expect("reason must be set")
            .contains("ended via abort"),
        "reason mirrors replay's abort refusal"
    );
}
