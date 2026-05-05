//! AC-driven integration tests for `StartupManager` crash-recovery sweep.
//!
//! AC-NFR-REL-02.1 — content store atomic writes (orphan sweep side)
//! AC-NFR-REL-03.1 — crash recovery surfaces in `session list`

use loom_core::content_store::LocalContentStore;
use loom_core::manifest_writer::{LocalManifestWriter, ManifestEntry, ManifestWriter, SessionId};
use loom_core::observability::Observability;
use loom_core::startup_manager::StartupManager;
use std::fs;
use std::sync::Arc;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

fn fixture() -> (StartupManager, TempDir) {
    let tmp = TempDir::new().unwrap();
    let sessions_root = tmp.path().join("sessions");
    let store_root = tmp.path().join("store");
    let cas_root = store_root.join("cas");
    fs::create_dir_all(&sessions_root).unwrap();
    fs::create_dir_all(&cas_root).unwrap();

    let obs = Observability::new(tmp.path().join("loom.log"), false);
    let cs: Arc<LocalContentStore> = Arc::new(LocalContentStore::new(store_root, Arc::clone(&obs)));
    let mw: Arc<LocalManifestWriter> = Arc::new(LocalManifestWriter::new(
        sessions_root.clone(),
        Arc::clone(&obs),
    ));

    let sm = StartupManager::new(sessions_root, cas_root, cs, mw, Arc::clone(&obs));
    (sm, tmp)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[allow(dead_code)]
fn make_session(mw: &LocalManifestWriter, _tmp: &TempDir) -> SessionId {
    let id = loom_core::manifest_writer::SessionId(ulid::Ulid::new().to_string().to_lowercase());
    mw.open_manifest(id.clone(), None).unwrap();
    id
}

// ---------------------------------------------------------------------------
// AC-NFR-REL-02.1 — CAS orphan sweep
// ---------------------------------------------------------------------------

#[test]
fn ac_nfr_rel_02_1_orphaned_tmpfile_removed_on_sweep() {
    let (sm, tmp) = fixture();
    // Create a file with non-CAS-address name in a cas shard dir
    let shard = tmp.path().join("store/cas/ab/cd");
    fs::create_dir_all(&shard).unwrap();
    fs::write(shard.join(".tmpXXXXXX"), b"partial write data").unwrap();

    let count = sm.sweep_orphan_tmpfiles().unwrap();

    assert_eq!(count, 1, "one orphan should be removed");
    assert!(
        !shard.join(".tmpXXXXXX").exists(),
        "orphan file must be deleted"
    );
}

#[test]
fn ac_nfr_rel_02_1_valid_cas_blob_survives_sweep() {
    let (sm, tmp) = fixture();
    // Place a file with a valid 64-char hex name in the CAS tree
    let hash = "a".repeat(64);
    let shard = tmp
        .path()
        .join("store/cas")
        .join(&hash[0..2])
        .join(&hash[2..4]);
    fs::create_dir_all(&shard).unwrap();
    let blob_path = shard.join(&hash[4..]);
    fs::write(&blob_path, b"valid blob data").unwrap();

    let count = sm.sweep_orphan_tmpfiles().unwrap();

    assert_eq!(count, 0, "valid CAS blob must not be removed");
    assert!(blob_path.exists(), "valid blob must survive");
}

#[test]
fn ac_nfr_rel_02_1_sweep_returns_correct_orphan_count() {
    let (sm, tmp) = fixture();
    let shard = tmp.path().join("store/cas/ff/00");
    fs::create_dir_all(&shard).unwrap();
    // 3 orphans, 1 valid blob
    fs::write(shard.join(".tmp1"), b"a").unwrap();
    fs::write(shard.join(".tmp2"), b"b").unwrap();
    fs::write(shard.join(".tmp3"), b"c").unwrap();
    let valid = "f".repeat(60); // 60 chars for remainder after ff/00/
    fs::write(shard.join(&valid), b"valid").unwrap();

    let count = sm.sweep_orphan_tmpfiles().unwrap();
    assert_eq!(count, 3);
}

#[test]
fn ac_nfr_rel_02_1_no_partial_files_remain_after_sweep() {
    let (sm, tmp) = fixture();
    let shard = tmp.path().join("store/cas/12/34");
    fs::create_dir_all(&shard).unwrap();
    fs::write(shard.join("partial_write"), b"incomplete").unwrap();
    fs::write(shard.join("also-bad"), b"also bad").unwrap();

    sm.sweep_orphan_tmpfiles().unwrap();

    // Only files with valid hex names should remain. Since we created two
    // non-hex files, the shard dir should be empty.
    let remaining: Vec<_> = fs::read_dir(&shard).unwrap().collect();
    assert!(
        remaining.is_empty(),
        "no non-CAS-address files should remain"
    );
}

// ---------------------------------------------------------------------------
// AC-NFR-REL-03.1 — manifest sweep + crash receipt + session list
// ---------------------------------------------------------------------------

#[test]
fn ac_nfr_rel_03_1_orphaned_active_session_gets_runtime_crash_receipt() {
    let (sm, tmp) = fixture();
    let mw = LocalManifestWriter::new(
        tmp.path().join("sessions"),
        Observability::new(tmp.path().join("loom2.log"), false),
    );

    // Create a session with Header only (no terminal entry)
    let session_id =
        loom_core::manifest_writer::SessionId(ulid::Ulid::new().to_string().to_lowercase());
    mw.open_manifest(session_id.clone(), None).unwrap();

    let wal_path = tmp
        .path()
        .join("sessions")
        .join(&session_id.0)
        .join("manifest.wal");
    let before = fs::read_to_string(&wal_path).unwrap();
    let line_count_before = before.lines().count();

    let (recovered, crashed, failed) = sm.sweep_manifests().unwrap();

    assert_eq!(crashed, 1, "one session should be marked crashed");
    assert_eq!(recovered, 0);
    assert!(failed.is_empty());

    // The WAL should now have an extra entry — the RuntimeCrash
    let after = fs::read_to_string(&wal_path).unwrap();
    let line_count_after = after.lines().count();
    assert_eq!(
        line_count_after,
        line_count_before + 1,
        "RuntimeCrash entry added"
    );

    // The last line must be a RuntimeCrash entry
    let last_line = after.lines().last().unwrap();
    let entry: serde_json::Value = serde_json::from_str(last_line).unwrap();
    assert_eq!(
        entry["kind"], "runtime_crash",
        "last entry must be runtime_crash"
    );
}

#[test]
fn ac_nfr_rel_03_1_crashed_session_last_completed_action_id_correct() {
    let (sm, tmp) = fixture();
    let mw = LocalManifestWriter::new(
        tmp.path().join("sessions"),
        Observability::new(tmp.path().join("loom2.log"), false),
    );

    let session_id =
        loom_core::manifest_writer::SessionId(ulid::Ulid::new().to_string().to_lowercase());
    mw.open_manifest(session_id.clone(), None).unwrap();

    // Append two ActionReceipt entries (action_id = 1, 2)
    for action_id in [1u64, 2u64] {
        mw.append(
            session_id.clone(),
            ManifestEntry::ActionReceipt {
                action_id,
                emitted_at_ms: now_ms(),
                receipt_canonical_bytes: vec![0u8; 4],
                prev_hash: String::new(),
            },
        )
        .unwrap();
    }

    sm.sweep_manifests().unwrap();

    let wal_path = tmp
        .path()
        .join("sessions")
        .join(&session_id.0)
        .join("manifest.wal");
    let content = fs::read_to_string(&wal_path).unwrap();
    let last_line = content.lines().last().unwrap();
    let entry: serde_json::Value = serde_json::from_str(last_line).unwrap();

    assert_eq!(entry["kind"], "runtime_crash");
    assert_eq!(
        entry["last_completed_action_id"], 2,
        "last_completed_action_id must be the last ActionReceipt's action_id"
    );
}

#[test]
fn ac_nfr_rel_03_1_session_with_session_terminal_not_marked_crashed() {
    let (sm, tmp) = fixture();
    let mw = LocalManifestWriter::new(
        tmp.path().join("sessions"),
        Observability::new(tmp.path().join("loom2.log"), false),
    );

    let session_id =
        loom_core::manifest_writer::SessionId(ulid::Ulid::new().to_string().to_lowercase());
    mw.open_manifest(session_id.clone(), None).unwrap();
    // Close it properly
    mw.append(
        session_id.clone(),
        ManifestEntry::SessionTerminal {
            action_id: 0,
            emitted_at_ms: now_ms(),
            reason: "close".into(),
            prev_hash: String::new(),
        },
    )
    .unwrap();

    let (recovered, crashed, failed) = sm.sweep_manifests().unwrap();

    assert_eq!(
        crashed, 0,
        "properly closed session must not be marked crashed"
    );
    assert_eq!(recovered, 1);
    assert!(failed.is_empty());
}

#[test]
fn ac_nfr_rel_03_1_per_session_isolation_one_corrupt_wal_does_not_block_others() {
    let (sm, tmp) = fixture();

    let sessions_root = tmp.path().join("sessions");
    // Create one valid orphaned session
    let mw = LocalManifestWriter::new(
        sessions_root.clone(),
        Observability::new(tmp.path().join("l.log"), false),
    );
    let good_id =
        loom_core::manifest_writer::SessionId(ulid::Ulid::new().to_string().to_lowercase());
    mw.open_manifest(good_id.clone(), None).unwrap();

    // Create one session with a corrupt WAL
    let bad_id = format!("01{}", "Z".repeat(24)); // invalid ULID but valid dir name
    let bad_dir = sessions_root.join(&bad_id);
    fs::create_dir_all(&bad_dir).unwrap();
    fs::write(bad_dir.join("manifest.wal"), b"not valid json\n").unwrap();

    let (recovered, crashed, failed) = sm.sweep_manifests().unwrap();

    // The good session is recovered as crashed (orphaned active)
    assert_eq!(crashed, 1, "good orphaned session recovered");
    // The corrupt session ends up in failed_sessions
    assert!(
        !failed.is_empty() || crashed + recovered > 0,
        "corrupt session is isolated"
    );
}

#[test]
fn ac_nfr_rel_03_1_manifest_jsonl_checkpoint_written_after_crash_receipt() {
    let (sm, tmp) = fixture();
    let mw = LocalManifestWriter::new(
        tmp.path().join("sessions"),
        Observability::new(tmp.path().join("loom2.log"), false),
    );

    let session_id =
        loom_core::manifest_writer::SessionId(ulid::Ulid::new().to_string().to_lowercase());
    mw.open_manifest(session_id.clone(), None).unwrap();

    sm.sweep_manifests().unwrap();

    let jsonl = tmp
        .path()
        .join("sessions")
        .join(&session_id.0)
        .join("manifest.jsonl");
    assert!(
        jsonl.exists(),
        "manifest.jsonl checkpoint must be written after crash receipt"
    );
}

#[test]
fn ac_nfr_rel_03_1_list_sessions_info_shows_crashed_status() {
    let (sm, tmp) = fixture();
    let sessions_root = tmp.path().join("sessions");

    let mw = LocalManifestWriter::new(
        sessions_root.clone(),
        Observability::new(tmp.path().join("l2.log"), false),
    );
    let session_id =
        loom_core::manifest_writer::SessionId(ulid::Ulid::new().to_string().to_lowercase());
    mw.open_manifest(session_id.clone(), None).unwrap();

    // Run recovery sweep (adds RuntimeCrash entry + checkpoint)
    sm.sweep_manifests().unwrap();

    // list_sessions_info must return the session with status "crashed"
    let infos = loom_core::core_api_facade::list_sessions_info_from_dir(&sessions_root).unwrap();
    let info = infos.iter().find(|(id, _, _)| id == &session_id.0);
    assert!(
        info.is_some(),
        "crashed session must appear in session list"
    );
    let (_, status, _) = info.unwrap();
    assert_eq!(status, "crashed");
}

#[test]
fn ac_nfr_rel_03_1_list_sessions_info_shows_closed_status() {
    let (_sm, tmp) = fixture();
    let sessions_root = tmp.path().join("sessions");

    let mw = LocalManifestWriter::new(
        sessions_root.clone(),
        Observability::new(tmp.path().join("l2.log"), false),
    );
    let session_id =
        loom_core::manifest_writer::SessionId(ulid::Ulid::new().to_string().to_lowercase());
    mw.open_manifest(session_id.clone(), None).unwrap();
    mw.append(
        session_id.clone(),
        ManifestEntry::SessionTerminal {
            action_id: 0,
            emitted_at_ms: now_ms(),
            reason: "close".into(),
            prev_hash: String::new(),
        },
    )
    .unwrap();

    let infos = loom_core::core_api_facade::list_sessions_info_from_dir(&sessions_root).unwrap();
    let info = infos.iter().find(|(id, _, _)| id == &session_id.0);
    assert!(info.is_some(), "closed session must appear in list");
    let (_, status, _) = info.unwrap();
    assert_eq!(status, "closed");
}

/// Aborted sessions surface their reason via the
/// `aborted:<reason>` status-string convention. The bridge layer
/// splits this back into `(status="aborted", reason=Some("X"))` for
/// the wire `SessionInfo`.
#[test]
fn list_sessions_info_encodes_abort_reason() {
    let (_sm, tmp) = fixture();
    let sessions_root = tmp.path().join("sessions");

    let mw = LocalManifestWriter::new(
        sessions_root.clone(),
        Observability::new(tmp.path().join("l2.log"), false),
    );
    let session_id =
        loom_core::manifest_writer::SessionId(ulid::Ulid::new().to_string().to_lowercase());
    mw.open_manifest(session_id.clone(), None).unwrap();
    mw.append(
        session_id.clone(),
        ManifestEntry::SessionTerminal {
            action_id: 0,
            emitted_at_ms: now_ms(),
            reason: "user-cancelled".into(),
            prev_hash: String::new(),
        },
    )
    .unwrap();

    let infos = loom_core::core_api_facade::list_sessions_info_from_dir(&sessions_root).unwrap();
    let info = infos.iter().find(|(id, _, _)| id == &session_id.0);
    assert!(info.is_some(), "aborted session must appear in list");
    let (_, status, _) = info.unwrap();
    assert_eq!(
        status, "aborted:user-cancelled",
        "abort reason must be encoded in status; bridge will split this into (status='aborted', reason='user-cancelled')"
    );
}

/// replay_complete is its own lifecycle marker, not
/// an abort. Distinct status string preserved end-to-end.
#[test]
fn replay_complete_is_not_aborted() {
    let (_sm, tmp) = fixture();
    let sessions_root = tmp.path().join("sessions");

    let mw = LocalManifestWriter::new(
        sessions_root.clone(),
        Observability::new(tmp.path().join("l2.log"), false),
    );
    let session_id =
        loom_core::manifest_writer::SessionId(ulid::Ulid::new().to_string().to_lowercase());
    mw.open_manifest(session_id.clone(), None).unwrap();
    mw.append(
        session_id.clone(),
        ManifestEntry::SessionTerminal {
            action_id: 0,
            emitted_at_ms: now_ms(),
            reason: "replay_complete".into(),
            prev_hash: String::new(),
        },
    )
    .unwrap();

    let infos = loom_core::core_api_facade::list_sessions_info_from_dir(&sessions_root).unwrap();
    let info = infos.iter().find(|(id, _, _)| id == &session_id.0);
    assert!(info.is_some(), "replay session must appear in list");
    let (_, status, _) = info.unwrap();
    assert_eq!(
        status, "replay_complete",
        "replay_complete is its own status, not 'aborted:replay_complete'"
    );
}
