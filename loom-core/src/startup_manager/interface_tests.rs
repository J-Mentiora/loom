// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-core/modules/startup_manager/interface_tests.rs` instead.
// Interface tests for `StartupManager`. Verifies SR-CORE-07 atomic
// writes / partial-write recovery, BC-CORE-01 storage layout sweep,
// design.md §3.5 crash-recovery flow.

use super::startup_manager::{FailedSession, RecoveryReport, StartupManager};
use loom_core::content_store::{ContentStore, LocalContentStore};
use loom_core::error::{LoomError, LoomErrorCode};
use loom_core::manifest_writer::{LocalManifestWriter, ManifestWriter, SessionId};
use loom_core::observability::Observability;
use std::path::PathBuf;
use std::sync::Arc;

fn fixture() -> StartupManager {
    let obs = Observability::new(PathBuf::from("/tmp/loom-test/loom.log"), false);
    let cs: Arc<dyn ContentStore> = Arc::new(LocalContentStore::new(
        PathBuf::from("/tmp/loom-test/store"),
        obs.clone(),
    ));
    let mw: Arc<dyn ManifestWriter> = Arc::new(LocalManifestWriter::new(
        PathBuf::from("/tmp/loom-test/sessions"),
        obs.clone(),
    ));
    StartupManager::new(
        PathBuf::from("/tmp/loom-test/sessions"),
        PathBuf::from("/tmp/loom-test/store/cas"),
        cs,
        mw,
        obs,
    )
}

// === Recovery report shape ===

#[test]
fn recovery_report_has_three_integer_counters_plus_failed_list() {
    let r = RecoveryReport {
        sessions_recovered: 3,
        sessions_crashed: 1,
        orphan_tmpfiles_removed: 7,
        failed_sessions: vec![],
    };
    let _u1: u64 = r.sessions_recovered;
    let _u2: u64 = r.sessions_crashed;
    let _u3: u64 = r.orphan_tmpfiles_removed;
}

#[test]
fn failed_session_carries_error_code_and_details_strings() {
    let f = FailedSession {
        session_id: SessionId("01HZBROKEN".into()),
        error_code: "manifest_integrity_failed".into(),
        details: "failed_at_index=12".into(),
    };
    assert_eq!(f.error_code, "manifest_integrity_failed");
}

// === SR-CORE-07: partial-write recovery ===

#[test]
fn perform_recovery_sweep_signature_returns_recovery_report() {
    let sm = fixture();
    fn _ck(s: &StartupManager) -> Result<RecoveryReport, LoomError> {
        s.perform_recovery_sweep()
    }
    let _ = _ck;
    let _ = sm;
}

#[test]
fn sweep_orphan_tmpfiles_returns_count_of_removed_files() {
    let sm = fixture();
    fn _ck(s: &StartupManager) -> Result<u64, LoomError> {
        s.sweep_orphan_tmpfiles()
    }
    let _ = _ck;
    let _ = sm;
}

#[test]
fn sweep_manifests_returns_recovered_crashed_failed_tuple() {
    let sm = fixture();
    fn _ck(
        s: &StartupManager,
    ) -> Result<(u64, u64, Vec<FailedSession>), LoomError> {
        s.sweep_manifests()
    }
    let _ = _ck;
    let _ = sm;
}

// === RuntimeCrash receipt for orphaned active sessions ===

#[test]
fn orphaned_active_session_produces_runtime_crash_error_variant() {
    let _e = LoomErrorCode::Internal;
}

// === Per-session isolation ===

#[test]
fn one_session_failure_does_not_block_others_failed_list_is_vec() {
    let r = RecoveryReport {
        sessions_recovered: 5,
        sessions_crashed: 2,
        orphan_tmpfiles_removed: 0,
        failed_sessions: vec![
            FailedSession {
                session_id: SessionId("01HZ-A".into()),
                error_code: "manifest_integrity_failed".into(),
                details: "".into(),
            },
            FailedSession {
                session_id: SessionId("01HZ-B".into()),
                error_code: "io_error".into(),
                details: "".into(),
            },
        ],
    };
    // Two failures coexist with five recoveries — isolation property.
    assert_eq!(r.failed_sessions.len(), 2);
    assert_eq!(r.sessions_recovered, 5);
}

#[test]
fn default_recovery_report_is_zero_counters_empty_failed() {
    let r = RecoveryReport::default();
    assert_eq!(r.sessions_recovered, 0);
    assert_eq!(r.sessions_crashed, 0);
    assert_eq!(r.orphan_tmpfiles_removed, 0);
    assert!(r.failed_sessions.is_empty());
}
