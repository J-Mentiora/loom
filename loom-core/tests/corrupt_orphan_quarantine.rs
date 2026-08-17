//! Regression tests for corrupt-WAL orphaned sessions (reap-corrupt-orphan-sessions).
//!
//! Bug: a session orphaned by a hard daemon kill can have a torn/partial final WAL write
//! that breaks its manifest hash chain. The recovery sweep's `process_session` validates
//! the chain BEFORE it can reconcile the orphan, so the `?` early-returns and the corrupt
//! session is left in `sessions_root` forever — it keeps reading as "active" to the disk
//! scanner and permanently consumes a `LOOM_MAX_CONCURRENT_SESSIONS` slot.
//!
//! These tests are written RED (Phase 2, TDD): they compile against the CURRENT API but
//! assert the DESIRED post-fix behavior, so they fail until Phase 3 lands the quarantine +
//! cap-hardening fixes. See specs/2026-06-04-reap-corrupt-orphan-sessions/plan.md.

use loom_core::content_store::LocalContentStore;
use loom_core::manifest_writer::{LocalManifestWriter, ManifestWriter, SessionId};
use loom_core::observability::Observability;
use loom_core::startup_manager::StartupManager;
use std::fs;
use std::io::Write;
use std::sync::Arc;
use tempfile::TempDir;

/// Build a StartupManager + a sibling ManifestWriter that share `sessions_root`,
/// so a test can both write WALs and run the sweep against the same tree.
fn fixture() -> (
    StartupManager,
    Arc<LocalManifestWriter>,
    std::path::PathBuf,
    TempDir,
) {
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
    let sm = StartupManager::new(
        sessions_root.clone(),
        cas_root,
        cs,
        Arc::clone(&mw) as Arc<dyn ManifestWriter>,
        Arc::clone(&obs),
    );
    (sm, mw, sessions_root, tmp)
}

/// Create an orphaned (non-terminal) session, then tear its final WAL write so the
/// hash chain is broken — exactly the hard-kill-mid-write failure mode.
fn make_corrupt_orphan(mw: &LocalManifestWriter, sessions_root: &std::path::Path) -> SessionId {
    let id = SessionId(ulid::Ulid::generate().to_string().to_lowercase());
    // Valid Header (chain root)…
    mw.open_manifest(id.clone(), None).unwrap();
    // …then a TORN trailing line (partial JSON) — the daemon was hard-killed mid-write.
    // No terminal marker, so this is an orphaned "active" session with a broken chain.
    let wal = sessions_root.join(&id.0).join("manifest.wal");
    let mut f = fs::OpenOptions::new().append(true).open(&wal).unwrap();
    write!(f, "{{\"kind\":\"action_receipt\",\"action_id\":2,\"prev_ha").unwrap(); // truncated, no newline
    f.flush().unwrap();
    id
}

/// DESIRED: the startup sweep moves a corrupt-WAL orphan OUT of `sessions_root` (quarantine),
/// so it no longer counts as an active session. FAILS today (the dir is left in place).
#[test]
fn corrupt_wal_orphan_is_quarantined_out_of_sessions_root() {
    let (sm, mw, sessions_root, tmp) = fixture();
    let id = make_corrupt_orphan(&mw, &sessions_root);

    let report = sm
        .perform_recovery_sweep()
        .expect("sweep should not error on a corrupt session");

    // The report counts the quarantine…
    assert_eq!(
        report.sessions_quarantined, 1,
        "recovery report should record the quarantined corrupt orphan"
    );
    // …the corrupt session must no longer live under sessions_root…
    assert!(
        !sessions_root.join(&id.0).join("manifest.wal").exists(),
        "corrupt orphan should be quarantined OUT of sessions_root, but its WAL is still there"
    );
    // …and it should be moved aside (non-destructive) to a sibling quarantine dir.
    let quarantine = tmp.path().join("quarantine").join(&id.0);
    assert!(
        quarantine.exists(),
        "corrupt orphan should be preserved under quarantine/ for forensics"
    );
}

/// DESIRED: the disk status scanner classifies a corrupt-WAL non-terminal session as
/// "corrupt" (a NON-active status), so the daemon cap stops counting it. FAILS today
/// (the torn line is silently skipped and the status defaults to "active").
#[test]
fn corrupt_wal_orphan_not_reported_active_by_disk_scanner() {
    let (_sm, mw, sessions_root, _tmp) = fixture();
    let id = make_corrupt_orphan(&mw, &sessions_root);

    let listed = loom_core::core_api_facade::list_sessions_info_from_dir(&sessions_root)
        .expect("scanner should not error");
    let status = listed
        .iter()
        .find(|(sid, _, _)| sid == &id.0)
        .map(|(_, s, _)| s.clone())
        .expect("corrupt session should still be listed by the scanner");

    assert_ne!(
        status, "active",
        "a corrupt-WAL non-terminal session must NOT count as active (it permanently eats a cap slot)"
    );
    assert_eq!(
        status, "corrupt",
        "the scanner should classify an unvalidatable WAL as \"corrupt\""
    );
}

/// D4: `quarantine_corrupt_sessions` must NOT quarantine a session in the live
/// `skip` set — a live session may simply be mid-WAL-write, and moving it would
/// disable a healthy session. (`reap` passes the live in-memory ids as `skip`.)
#[test]
fn reap_skips_live_sessions() {
    use std::collections::HashSet;
    let (sm, mw, sessions_root, _tmp) = fixture();
    let id = make_corrupt_orphan(&mw, &sessions_root);

    let mut skip = HashSet::new();
    skip.insert(id.clone());
    let outcome = sm
        .quarantine_corrupt_sessions(false, &skip)
        .expect("reap should not error");

    assert_eq!(outcome.skipped_live, 1, "live session should be skipped");
    assert!(
        outcome.quarantined.is_empty(),
        "a live (skipped) session must never be quarantined"
    );
    assert!(
        sessions_root.join(&id.0).join("manifest.wal").exists(),
        "the live session's dir must be left untouched"
    );
}

/// Dry-run previews the corrupt orphan without moving anything.
#[test]
fn reap_dry_run_does_not_move() {
    use std::collections::HashSet;
    let (sm, mw, sessions_root, _tmp) = fixture();
    let id = make_corrupt_orphan(&mw, &sessions_root);

    let outcome = sm
        .quarantine_corrupt_sessions(true, &HashSet::new())
        .expect("dry-run should not error");

    assert!(outcome.dry_run);
    assert_eq!(outcome.quarantined.len(), 1, "candidate should be detected");
    assert!(
        sessions_root.join(&id.0).join("manifest.wal").exists(),
        "dry-run must NOT move the corrupt session"
    );
}

/// A healthy orphan (valid Header, no terminal, intact chain) is NOT corrupt —
/// it should be reconciled by the normal RuntimeCrash path, never quarantined.
#[test]
fn healthy_orphan_is_not_quarantined() {
    let (sm, mw, sessions_root, tmp) = fixture();
    let id = SessionId(ulid::Ulid::generate().to_string().to_lowercase());
    mw.open_manifest(id.clone(), None).unwrap(); // valid Header only, intact chain

    let report = sm.perform_recovery_sweep().expect("sweep ok");

    assert_eq!(
        report.sessions_quarantined, 0,
        "healthy orphan must not be quarantined"
    );
    assert_eq!(
        report.sessions_crashed, 1,
        "healthy orphan gets a RuntimeCrash"
    );
    assert!(
        !tmp.path().join("quarantine").join(&id.0).exists(),
        "healthy orphan must not appear in quarantine/"
    );
    assert!(
        sessions_root.join(&id.0).join("manifest.wal").exists(),
        "healthy orphan stays in sessions_root (reconciled in place)"
    );
}

/// A quarantine move that can't complete (here: the destination already exists)
/// is recorded in `failed` and the session is LEFT in place — never silently
/// dropped. (Mirrors the EXDEV cross-device path, which is hard to unit-test.)
#[test]
fn quarantine_failure_is_recorded_not_silently_dropped() {
    use std::collections::HashSet;
    let (sm, mw, sessions_root, tmp) = fixture();
    let id = make_corrupt_orphan(&mw, &sessions_root);

    // Pre-create the destination so the rename can't land.
    let dest = tmp.path().join("quarantine").join(&id.0);
    fs::create_dir_all(&dest).unwrap();

    let outcome = sm
        .quarantine_corrupt_sessions(false, &HashSet::new())
        .expect("reap should not hard-error on a per-session failure");

    assert!(
        outcome.quarantined.is_empty(),
        "the blocked session must not count as quarantined"
    );
    assert_eq!(outcome.failed.len(), 1, "the failure must be recorded");
    assert_eq!(outcome.failed[0].session_id.0, id.0);
    assert!(
        sessions_root.join(&id.0).join("manifest.wal").exists(),
        "a failed quarantine must leave the session in place"
    );
}
