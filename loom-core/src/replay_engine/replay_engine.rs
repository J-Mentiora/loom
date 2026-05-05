// ReplayEngine — bit-equal structural replay.
//
// # Contract semantics
// - **Reads CAS only.** During replay,
//   `DeterminismHarness::install_replay_mode(tape)` swaps clock_now /
//   rng_next / net_request to tape-driven implementations. The surface
//   .wasm cannot reach live network.
// - **Replay ≥ 5× real-time.** No virtual-clock real-time
//   honoring — tape pops happen at WASM-call rate.
// - **Bit-equal structural.** Hash chain
//   compared between source and replay, screenshots excluded from
//   `differences[]`.
// - **Replay missing blob.** Tape references a content_ref not in CAS →
//   `ReplayMissingBlob { missing_hash, kind }`.

use loom_core::content_store::ContentStore;
use loom_core::determinism_harness::DeterminismHarness;
use loom_core::error::LoomError;
use loom_core::manifest_writer::{ManifestWriter, SessionId};
use loom_core::observability::Observability;
use loom_core::session_manager::LocalSessionManager;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayOpts {
    /// If true, exclude screenshots from comparison (default: true).
    pub exclude_screenshots: bool,
    /// Per-action wall-clock budget for the replay run.
    pub action_walltime_budget_ms: u64,
}

impl Default for ReplayOpts {
    fn default() -> Self {
        Self {
            exclude_screenshots: true,
            action_walltime_budget_ms: 5_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffOpts {
    pub exclude_screenshots: bool,
    pub include_audit_entries: bool,
}

/// One field-level difference between two manifest entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDiff {
    pub action_id: u64,
    pub field_path: String,
    pub source_value: String,
    pub replay_value: String,
}

/// Replay report. Returned to `loom-rpc` after `replay()` completes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayReport {
    pub source_session_id: SessionId,
    pub replay_session_id: SessionId,
    pub actions_compared: u64,
    pub differences: Vec<FieldDiff>,
    pub screenshots_diff_count: u64,
}

/// Diff report. Returned by `diff()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffReport {
    pub a: SessionId,
    pub b: SessionId,
    pub action_count_delta: i64,
    pub field_diffs: Vec<FieldDiff>,
    pub screenshot_diffs: Vec<u64>,
}

/// Validation result. Returned by `validate()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    pub session_id: String,
    pub passed: bool,
    pub reasons: Vec<String>,
}

/// Concrete ReplayEngine implementation.
pub struct LocalReplayEngine {
    pub(crate) content_store: Arc<dyn ContentStore>,
    pub(crate) manifest_writer: Arc<dyn ManifestWriter>,
    pub(crate) determinism: Arc<DeterminismHarness>,
    pub(crate) obs: Arc<Observability>,
    pub(crate) session_manager: Arc<LocalSessionManager>,
    pub(crate) sessions_root: PathBuf,
}

impl LocalReplayEngine {
    pub fn new(
        content_store: Arc<dyn ContentStore>,
        manifest_writer: Arc<dyn ManifestWriter>,
        determinism: Arc<DeterminismHarness>,
        obs: Arc<Observability>,
        session_manager: Arc<LocalSessionManager>,
        sessions_root: PathBuf,
    ) -> Self {
        Self {
            content_store,
            manifest_writer,
            determinism,
            obs,
            session_manager,
            sessions_root,
        }
    }
}

/// Public trait surface (per `loom-core_contract.md`).
pub trait ReplayEngine: Send + Sync {
    /// Replay a recorded session. Returns the new replay session id.
    /// Pre: source manifest is intact (validate_manifest_integrity = Ok).
    /// Post: new session manifest is bit-equal structurally to source.
    /// Errors: ReplayMissingBlob { missing_hash, kind },
    ///         ManifestIntegrityFailed { failed_at_index }.
    fn replay(&self, source: SessionId, opts: ReplayOpts) -> Result<SessionId, LoomError>;

    /// Diff two session manifests. Screenshots excluded from `differences`
    /// count by default (Hard binding 5).
    fn diff(&self, a: SessionId, b: SessionId, opts: DiffOpts) -> Result<DiffReport, LoomError>;
}

// ReplayEngine impl is in src/loom-core/src/replay_engine/impl_replay.rs
