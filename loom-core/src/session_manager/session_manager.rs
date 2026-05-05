// SessionManager — session lifecycle FSM owner.
//
// # Contract semantics
// - **FSM:** `Created → Active → Closed | Aborted | Killed | Crashed`.
//   Transitions guarded by per-session `Mutex<SessionStatus>`.
// - **Per-session tokio task.** Each session runs on a
//   dedicated multi-threaded tokio task with its own arena.
// - **Abort propagation.** Each session owns
//   `Arc<Notify>` + `Arc<AtomicBool>`. `abort()` flips the bool and
//   calls `notify.notify_one()`. Host-fn entries check the bool;
//   `tokio::select!` races real call vs notify.
// - **Warm create budget.** ULID gen + ensure_dir + WAL
//   header append + fsync + task spawn. NO WASM/Chromium/network on the
//   sync path.
// - **Kill-callback registration.** `create()` registers an
//   `Arc<dyn Fn(SessionId, KillReason)>` into `BudgetEnforcer` so a
//   budget breach can interrupt this session's task without a
//   structural cycle.

use loom_core::budget_enforcer::{BudgetEnforcer, BudgetLimits, KillReason, SessionCounters};
use loom_core::content_store::ContentStore;
use loom_core::determinism_harness::{DeterminismHarness, TapeWriter};
use loom_core::error::LoomError;
use loom_core::manifest_writer::{ManifestWriter, SessionId, WriterHandle};
use loom_core::observability::Observability;
use loom_core::vault::Vault;
use loom_shared::types::{EpochMs, Seed};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use tokio::sync::Notify;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Created,
    Active,
    Closed,
    Aborted,
    Killed,
    Crashed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbortReason {
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCreateOpts {
    pub agent_id: String,
    pub surface: String,
    pub seed: Option<u64>,
    pub limits: Option<BudgetLimits>,
    pub replay_of: Option<SessionId>,
    /// When set (replay path), forces the manifest Header's
    /// `started_at_ms` to this exact value instead of `now_ms()`.
    /// Required for hash-chain bit-equality: the chain
    /// hashes over the canonical Header bytes, so a divergent
    /// timestamp poisons every subsequent prev_hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms_override: Option<u64>,
    /// Operator-supplied `--capture-policy`.
    /// Wire form `"minimal" | "default" | "full"`; `None` means the
    /// server-default profile applies. Persisted in the manifest Header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_policy: Option<String>,
    /// Operator's `--no-blocklist` opt-out.
    /// Default `false` (the analytics/ads/telemetry blocklist enforces
    /// sub-resource gating on every navigate). Persisted on per-session
    /// state so the host's `navigate_execute` can compute
    /// `blocklist_enabled = !no_blocklist` for each ShimRequest.
    #[serde(default)]
    pub no_blocklist: bool,
    /// Operator's `--profile` choice — `"safe" | "standard" | "full"`. Wire-default
    /// is `"safe"` (per `loom_rpc::core_service_adapter::CreateSessionParams::default_profile`).
    /// Daemon's evaluate gate and shim's download confinement
    /// both branch on this value.
    #[serde(default = "default_profile_string")]
    pub profile: String,
}

fn default_profile_string() -> String {
    "safe".to_string()
}

impl Default for SessionCreateOpts {
    fn default() -> Self {
        Self {
            agent_id: String::new(),
            surface: String::new(),
            seed: None,
            limits: None,
            replay_of: None,
            started_at_ms_override: None,
            capture_policy: None,
            no_blocklist: false,
            profile: default_profile_string(),
        }
    }
}

/// Internal session lifecycle error. Mapped to `LoomError` at the facade
/// boundary via `From<SessionError> for LoomError`.
#[derive(Debug)]
pub enum SessionError {
    SessionUnknown {
        session_id: String,
    },
    SessionAlreadyClosed {
        session_id: String,
    },
    SessionAborted {
        reason: String,
    },
    SessionKilled {
        reason: String,
    },
    /// Profile fields are immutable after creation.
    SessionProfileImmutable,
    SessionClosed {
        closed_at_ms: u64,
    },
    ManifestError(LoomError),
}

impl From<SessionError> for LoomError {
    fn from(e: SessionError) -> LoomError {
        use loom_core::error::LoomErrorCode;
        match e {
            SessionError::SessionUnknown { session_id } => LoomError::new(
                LoomErrorCode::SessionNotFound,
                format!("session not found: {session_id}"),
            ),
            SessionError::SessionAlreadyClosed { session_id } => LoomError::new(
                LoomErrorCode::SessionAlreadyClosed,
                format!("session already closed: {session_id}"),
            ),
            SessionError::SessionAborted { reason } => LoomError::new(
                LoomErrorCode::SessionAborted,
                format!("session aborted: {reason}"),
            ),
            SessionError::SessionKilled { reason } => LoomError::new(
                LoomErrorCode::SessionKilled,
                format!("session killed: {reason}"),
            ),
            SessionError::SessionProfileImmutable => LoomError::new(
                LoomErrorCode::InvalidArgument,
                "session profile is immutable after creation",
            ),
            SessionError::SessionClosed { closed_at_ms } => LoomError::new(
                LoomErrorCode::SessionAlreadyClosed,
                format!("session closed at {closed_at_ms}"),
            ),
            SessionError::ManifestError(e) => e,
        }
    }
}

/// Per-session in-memory state. Owned via `Arc` by the session table.
pub struct Session {
    pub id: SessionId,
    /// Guarded by a sync mutex — transitions never cross await points.
    pub status: parking_lot::Mutex<SessionStatus>,
    pub abort_flag: Arc<AtomicBool>,
    pub abort_notify: Arc<Notify>,
    pub writer: Arc<WriterHandle>,
    pub counters: Arc<SessionCounters>,
    pub tape_writer: Arc<tokio::sync::Mutex<TapeWriter>>,
    pub task_handle: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Per-session monotonic action sequence, 0-based. Incremented atomically
    /// at action dispatch via `allocate_action_id()`.
    /// In-memory only — NOT persisted across daemon restarts (the daemon
    /// today doesn't resume Active sessions across restarts; sessions become
    /// Crashed on restart per startup_manager). See decisions.md Q3.
    /// TODO(action-id-resume): rebuild from last ActionReceipt.action_id+1
    /// if a future feature adds session-resume across restarts.
    pub next_action_id: AtomicU64,
    /// Budget kill metadata. Written by the kill-callback BEFORE the
    /// abort_flag is flipped + abort_notify is fired. The session
    /// executor's `tokio::select!` abort-arm reads this to distinguish
    /// budget-driven kills (→ `ActionOutcome::Trapped { BudgetExceeded }`)
    /// from user-initiated aborts (→ `ActionOutcome::Aborted`).
    pub kill_reason: Arc<parking_lot::Mutex<Option<KillReason>>>,
    /// Handle to the per-session wall-clock timer task spawned at
    /// `create()`. Cancelled on `close()` / `abort()` so a closed session
    /// never trips the kill callback after lifecycle exit.
    /// `None` for replay sessions and for sessions created without a
    /// positive `session_walltime_ms` budget.
    pub budget_timer: parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Capture-policy wire-form ("minimal" / "default" / "full"), as
    /// passed in `SessionCreateOpts` and persisted to the manifest
    /// header. The daemon reads this at dispatch time
    /// to apply per-receipt capture-policy via
    /// `loom_rpc::host_service_adapter::wire_capture::apply_capture_profile_to_wire`.
    /// `None` means default profile.
    pub capture_policy: Option<String>,
    /// Operator's `--no-blocklist` opt-out.
    /// `false` by default (blocklist enforced). Read by the host's
    /// `navigate_execute` to compute the per-PageNavigate
    /// `blocklist_enabled` field.
    pub no_blocklist: bool,
    /// Per-session determinism seed. The `Option<u64> → Seed` collapse
    /// happens exactly once, at `LocalSessionManager::create` —
    /// `opts.seed.unwrap_or(default_seed)`. Downstream layers (HostState,
    /// shim wire, target_manager, determinism_injector, JS template)
    /// carry concrete `Seed(u64)` only.
    pub seed: Seed,
    /// Per-session Unix epoch milliseconds. Substituted into the shim
    /// JS template's `Date.now` constant. Defaults to `now_ms()` at
    /// session create when `opts.started_at_ms_override` is not set,
    /// so two sessions with the same seed but different create-times
    /// get different `Date.now()` outputs (which is correct — the seed
    /// only determines RNG, not clock).
    pub epoch_ms: EpochMs,
    /// Operator's `--profile` choice persisted at create. Read by daemon's
    /// WasmBridge for the evaluate gate AND by
    /// host_function_table when spawning the per-session shim (downloads
    /// dir env var injection). Immutable after create.
    pub profile: String,
    /// Session-scoped downloads directory (`~/.loom/sessions/<ulid>/downloads/`).
    /// Populated only when `profile == "safe"` — Chromium uses this as the
    /// `downloadPath` for `Browser.setDownloadBehavior(allowAndName)` so all
    /// downloads stay confined to the session dir.
    /// `None` for non-safe profiles or pre-fix sessions.
    pub downloads_dir: Option<std::path::PathBuf>,
}

impl Session {
    /// Allocate the next monotonic action_id for this session.
    /// Returns the value the action should carry (0-based); the counter
    /// is then advanced to N+1. Relaxed ordering is sufficient: per-session
    /// dispatch is serialized today by the daemon's WasmBridge, and
    /// `fetch_add` is atomic regardless of ordering.
    pub fn allocate_action_id(&self) -> u64 {
        self.next_action_id.fetch_add(1, Ordering::Relaxed)
    }
}

/// Concrete SessionManager implementation.
pub struct LocalSessionManager {
    pub(crate) sessions: dashmap::DashMap<SessionId, Arc<Session>>,
    pub(crate) content_store: Arc<dyn ContentStore>,
    pub(crate) manifest_writer: Arc<dyn ManifestWriter>,
    pub(crate) vault: Arc<dyn Vault>,
    pub(crate) budget_enforcer: Arc<dyn BudgetEnforcer>,
    pub(crate) determinism: Arc<DeterminismHarness>,
    pub(crate) obs: Arc<Observability>,
    /// Fallback seed for sessions created without an explicit
    /// `SessionCreateOpts.seed`. Read once from the daemon config at
    /// startup. The `Option<u64> → Seed` collapse for `opts.seed.is_none()`
    /// happens exactly once, in `LocalSessionManager::create`.
    pub(crate) default_seed: u64,
    /// Sessions root directory (`<data_root>/sessions/`). Used by `create()`
    /// to build the per-session downloads directory under safe profile
    /// (`<sessions_root>/<ulid>/downloads/`).
    pub(crate) sessions_root: std::path::PathBuf,
    /// Weak self-pointer set at `new()` via `Arc::new_cyclic`. Used by
    /// `create()` to build the budget kill-callback closure without
    /// taking `Arc<Self>` as a receiver (interface_tests assert
    /// `create(&self)`). The closure upgrades the Weak on every fire to
    /// avoid a static dep cycle (BudgetEnforcer → SessionManager → ...).
    pub(crate) me: Weak<Self>,
}

impl LocalSessionManager {
    // 8 args is one over clippy's threshold. Refactoring to a Config
    // struct would reshape every call site (tests + benchmarks +
    // CoreApiFacade) for no semantic gain — the constructor stays
    // dependency-injected per the existing pattern.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        content_store: Arc<dyn ContentStore>,
        manifest_writer: Arc<dyn ManifestWriter>,
        vault: Arc<dyn Vault>,
        budget_enforcer: Arc<dyn BudgetEnforcer>,
        determinism: Arc<DeterminismHarness>,
        obs: Arc<Observability>,
        default_seed: u64,
        sessions_root: std::path::PathBuf,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            sessions: dashmap::DashMap::new(),
            content_store,
            manifest_writer,
            vault,
            budget_enforcer,
            determinism,
            obs,
            default_seed,
            sessions_root,
            me: me.clone(),
        })
    }

    // create(), get(), close(), abort(), abort_all() — implemented in impl_local.rs

    /// Internal helper: build the kill-callback closure registered with
    /// `BudgetEnforcer`. The closure interrupts the session's tokio task
    /// via abort_notify + abort_flag without creating a static dep cycle.
    ///
    /// Idempotent — the BudgetEnforcer's per-session `killed: AtomicBool`
    /// already gates against double-fire, but the closure also tolerates
    /// being invoked twice on the same session (e.g. by separate budget
    /// kinds): `kill_reason` is only set on first fire and the status
    /// transition only happens from `Active`/`Created` (later transitions
    /// no-op).
    pub fn kill_callback_for(
        &self,
        _id: SessionId,
    ) -> Arc<dyn Fn(SessionId, KillReason) + Send + Sync> {
        let weak = self.me.clone();
        Arc::new(move |sid, reason| {
            let manager = match weak.upgrade() {
                Some(m) => m,
                None => return,
            };
            let session = match manager.sessions.get(&sid) {
                Some(s) => Arc::clone(s.value()),
                None => return,
            };

            // Capture a kind-tag for the manifest entry before move.
            let kind_tag: &'static str = match &reason {
                KillReason::BudgetExceeded { kind, .. } => match kind {
                    loom_core::budget_enforcer::ResourceKind::Walltime => "wall_clock",
                    loom_core::budget_enforcer::ResourceKind::Network => "network",
                    loom_core::budget_enforcer::ResourceKind::DomNodes => "dom_nodes",
                    loom_core::budget_enforcer::ResourceKind::JsHeap => "js_heap",
                },
                KillReason::UserAbort => "user_abort",
                KillReason::StoreFailure => "store_failure",
            };

            // Write kill_reason BEFORE flipping abort_flag — the
            // executor's abort arm reads kill_reason to disambiguate
            // budget-kill from user-abort.
            {
                let mut slot = session.kill_reason.lock();
                if slot.is_some() {
                    // Idempotent: a second kill (e.g. wall-clock + network
                    // racing) doesn't overwrite the first reason. The
                    // first kill always wins.
                    return;
                }
                *slot = Some(reason);
            }

            session.abort_flag.store(true, Ordering::Release);
            session.abort_notify.notify_one();

            // Idempotent status transition: only flip if currently Active
            // or Created. Already-terminal sessions stay terminal.
            {
                let mut status = session.status.lock();
                if matches!(*status, SessionStatus::Active | SessionStatus::Created) {
                    *status = SessionStatus::Killed;
                }
            }

            // Append a SessionTerminal manifest entry tagged with the
            // budget kind. Best-effort: a write failure here can't undo
            // the kill so we ignore the error (caller already aborted).
            let _ = manager.manifest_writer.append(
                sid,
                loom_core::manifest_writer::ManifestEntry::SessionTerminal {
                    action_id: 0,
                    emitted_at_ms: now_ms(),
                    reason: format!("budget_exceeded:{kind_tag}"),
                    prev_hash: String::new(),
                },
            );
        })
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
