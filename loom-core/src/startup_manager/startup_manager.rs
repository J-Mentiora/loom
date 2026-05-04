// StartupManager — daemon crash-recovery sweep.
//
// # Contract semantics
// - **Run on daemon startup, before accepting any RPC traffic.** Returns
//   `RecoveryReport`; the daemon refuses new sessions if any sweep failed.
// - **CAS sweep.** Walk `~/.../store/cas/`; delete orphaned `O_TMPFILE`
//   artifacts (failed `linkat` operations).
// - **Manifest sweep.** For each `sessions/<ulid>/`:
//   - If `manifest.wal` is non-empty, replay all WAL entries into
//     `manifest.jsonl` and truncate the WAL.
//   - Re-read the consistent `manifest.jsonl`; validate the hash chain.
//   - If the last entry's status is `active`, append a `RuntimeCrash {
//     last_completed_action_id }` receipt and mark session `crashed`.
// - **Per-session isolation.** Each session's recovery is independent;
//   one failure does not block other sessions.

use loom_core::content_store::ContentStore;
use loom_core::manifest_writer::{ManifestWriter, SessionId};
use loom_core::observability::Observability;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecoveryReport {
    pub sessions_recovered: u64,
    pub sessions_crashed: u64,
    pub orphan_tmpfiles_removed: u64,
    pub failed_sessions: Vec<FailedSession>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedSession {
    pub session_id: SessionId,
    pub error_code: String,
    pub details: String,
}

pub struct StartupManager {
    pub(crate) sessions_root: PathBuf,
    pub(crate) cas_root: PathBuf,
    pub(crate) content_store: Arc<dyn ContentStore>,
    pub(crate) manifest_writer: Arc<dyn ManifestWriter>,
    pub(crate) obs: Arc<Observability>,
}

impl StartupManager {
    pub fn new(
        sessions_root: PathBuf,
        cas_root: PathBuf,
        content_store: Arc<dyn ContentStore>,
        manifest_writer: Arc<dyn ManifestWriter>,
        obs: Arc<Observability>,
    ) -> Self {
        Self {
            sessions_root,
            cas_root,
            content_store,
            manifest_writer,
            obs,
        }
    }
    // perform_recovery_sweep / sweep_orphan_tmpfiles / sweep_manifests
    // are implemented in startup_manager/impl_local.rs
}
