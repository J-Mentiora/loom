// ManifestWriter — append-only WAL with hash-chained checkpoint.
//
// # Contract semantics
// - Storage layout: `sessions/<ulid>/manifest.jsonl`
//   (checkpointed) + `sessions/<ulid>/manifest.wal` (WAL, fsynced on
//   every append). Checkpoint cadence: 100 entries OR 1 second.
// - Hash-chain: every append computes
//   `prev_hash = sha256(serde_jcs::to_string(prev_entry))` over canonical
//   JSON. **`serde_json::to_string` is BANNED in this module** (clippy
//   `disallowed_methods`).
// - Audit entries: `Vault` and other modules write typed
//   audit entries through `append_audit`; they share the SAME hash chain
//   as action receipts.
// - All `ManifestEntry` variants have integer-only fields per HARD #3.

use loom_core::budget_enforcer::BudgetLimits;
use loom_core::error::LoomError;
use loom_core::observability::Observability;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// Session ID. ULID, 26-char Crockford base32 lowercase. Defined in WIT,
/// but a Rust newtype is permitted as a thin wrapper.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SessionId(pub String);

/// A single line in `manifest.jsonl`. Tagged enum; serialized via JCS.
/// All numeric fields are integers (BC HARD #3 / Hybrid synthesis HARD #3).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ManifestEntry {
    /// First entry of a manifest. `prev_hash` is None on the header.
    Header {
        session_id: String,
        started_at_ms: u64,
        prev_hash: Option<String>,
        /// Session budget limits at creation time.
        /// None means all defaults apply.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        budgets: Option<BudgetLimits>,
        /// Operator's `--capture-policy` choice.
        /// Wire form: `"minimal" | "default" | "full"`. `None` means the
        /// server-default profile applied — kept skip-if-none so legacy
        /// (pre-feature) headers round-trip without schema churn.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        capture_policy: Option<String>,
    },
    /// A surface action receipt. Real Receipt type is WIT-derived; this
    /// is the manifest-side wrapper.
    ActionReceipt {
        action_id: u64,
        emitted_at_ms: u64,
        receipt_canonical_bytes: Vec<u8>,
        prev_hash: String,
    },
    /// An audit entry (Vault grant lifecycle, FSM transitions).
    AuditEntry {
        action_id_ref: Option<u64>,
        emitted_at_ms: u64,
        audit_kind: AuditKind,
        canonical_bytes: Vec<u8>,
        prev_hash: String,
    },
    /// Terminal kill receipt (budget breach, abort, close).
    SessionTerminal {
        action_id: u64,
        emitted_at_ms: u64,
        reason: String,
        prev_hash: String,
    },
    /// Crash-recovery sentinel written by StartupManager.
    RuntimeCrash {
        last_completed_action_id: u64,
        emitted_at_ms: u64,
        prev_hash: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditKind {
    GrantIssued,
    GrantConsumed,
    GrantExpired,
    GrantRevoked,
    GrantRejected,
    FsmTransition,
    /// Sub-resource request blocked by the default network blocklist.
    /// Canonical bytes carry `{matched_pattern, reason, url}` (JCS sorts
    /// keys at encode).
    BlockedUrl,
}

/// Per-session writer handle. Owned by `Session` in `SessionManager`.
#[derive(Debug, Clone)]
pub struct WriterHandle {
    pub session_id: SessionId,
    pub(crate) wal_path: PathBuf,
    pub(crate) checkpoint_path: PathBuf,
}

/// Concrete implementation. Held by `CoreApiFacade` as `Arc<LocalManifestWriter>`.
pub struct LocalManifestWriter {
    pub(crate) sessions_root: PathBuf,
    pub(crate) checkpoint_every_n: u64,
    pub(crate) obs: Arc<Observability>,
}

impl LocalManifestWriter {
    pub fn new(sessions_root: PathBuf, obs: Arc<Observability>) -> Self {
        Self {
            sessions_root,
            checkpoint_every_n: 100,
            obs,
        }
    }

    // open_manifest() — implemented in impl_local.rs
}

/// Public trait surface (per `loom-core_contract.md`).
pub trait ManifestWriter: Send + Sync {
    /// Open or create the WAL for a session. Writes the Header entry on
    /// first creation; opens in append mode for resumed sessions.
    /// `budgets` is stored in the Header; pass None for defaults.
    fn open_manifest(
        &self,
        session: SessionId,
        budgets: Option<BudgetLimits>,
    ) -> Result<WriterHandle, LoomError> {
        self.open_manifest_with_started_at(session, budgets, None, None)
    }

    /// Same as `open_manifest`, but lets the caller override the
    /// Header's `started_at_ms`. Used by the replay path:
    /// replay reuses the original session's
    /// `started_at_ms` so the manifest hash chain — which chains over
    /// the canonical Header bytes — is bit-equal to the source's
    /// chain. Pass `None` to use `now_ms()` (default behaviour).
    fn open_manifest_with_started_at(
        &self,
        session: SessionId,
        budgets: Option<BudgetLimits>,
        started_at_ms_override: Option<u64>,
        capture_policy: Option<String>,
    ) -> Result<WriterHandle, LoomError>;

    /// Append an entry. Atomic: WAL write + fsync; checkpoint to
    /// `manifest.jsonl` every `checkpoint_every_n` entries OR 1s.
    /// Pre: session is Active. Post: hash chain extended.
    fn append(&self, session: SessionId, entry: ManifestEntry) -> Result<(), LoomError>;

    /// Append a Vault audit entry. Same hash chain as
    /// `append`; distinguished only by the `AuditEntry` variant tag.
    fn append_audit(
        &self,
        session: SessionId,
        kind: AuditKind,
        canonical_bytes: Vec<u8>,
    ) -> Result<(), LoomError>;

    /// Validate hash chain integrity for a session.
    /// Returns Ok(()) if intact, Err(ManifestCorrupt) on chain break.
    fn validate(&self, session: SessionId) -> Result<(), LoomError>;

    /// Write the WAL contents as a stable `manifest.jsonl` checkpoint.
    /// Called by `StartupManager` after appending a `RuntimeCrash` entry.
    fn checkpoint(&self, session: SessionId) -> Result<(), LoomError>;
}

// ManifestWriter impl for LocalManifestWriter lives in impl_local.rs
