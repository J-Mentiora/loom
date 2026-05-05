// BudgetEnforcer — per-session resource accounting.
//
// # Contract semantics
// - **Two-phase (IC-CORE-06).** `check()` runs BEFORE host-fn dispatch;
//   `account()` runs AFTER side-effect emission. Both can return Exceeded.
// - **Per-session AtomicU64 (SR-CORE-16).** Each session owns its own
//   counter struct; `account()` calls `fetch_add(Ordering::Relaxed)`.
// - **Kill-callback breaks the structural cycle.** `BudgetEnforcer`
//   does NOT depend on `SessionManager`. Instead, `SessionManager`
//   registers `register_kill_callback(SessionId, Arc<dyn Fn>)` at
//   session creation. On Exceeded, `BudgetEnforcer` invokes the
//   callback synchronously. (See module_list cycle-break notes.)
// - **One path per resource kind.** Walltime accounted by the tokio
//   timer driver; network bytes by Vault::substitute / net_request
//   post-response; dom nodes by Chromium shim; js heap by V8 snapshot.

use loom_core::error::LoomError;
use loom_core::manifest_writer::SessionId;
use loom_core::observability::Observability;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Walltime,
    Network,
    DomNodes,
    JsHeap,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KillReason {
    BudgetExceeded {
        kind: ResourceKind,
        observed: u64,
        limit: u64,
    },
    UserAbort,
    StoreFailure,
}

/// Per-session budget limits. Defaults applied at session creation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BudgetLimits {
    /// Maximum cumulative wall-clock for the whole session (ms). Default 600 s.
    /// AC-BUDGET-01.1: kill session when session total exceeds this.
    pub session_walltime_ms: u64,
    /// Maximum wall-clock for a single action (ms). Default 60 s.
    /// AC-BUDGET-01.2: pre-reject action when estimated time exceeds this.
    pub action_walltime_ms: u64,
    pub network_bytes: u64,
    pub dom_nodes: u64,
    pub js_heap_bytes: u64,
}

impl Default for BudgetLimits {
    /// Soft-binding defaults: 600s session wall / 60s action wall / 50 MB net / 50k DOM / 512 MB heap.
    fn default() -> Self {
        Self {
            session_walltime_ms: 600_000,
            action_walltime_ms: 60_000,
            network_bytes: 50 * 1024 * 1024,
            dom_nodes: 50_000,
            js_heap_bytes: 512 * 1024 * 1024,
        }
    }
}

/// Per-session counter struct. Owned by the session via `Arc`;
/// `BudgetEnforcer` reads through references.
#[derive(Debug, Default)]
pub struct SessionCounters {
    pub walltime_ms: AtomicU64,
    pub network_bytes: AtomicU64,
    pub dom_nodes: AtomicU64,
    pub js_heap_bytes: AtomicU64,
}

impl SessionCounters {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub fn snapshot(&self) -> (u64, u64, u64, u64) {
        (
            self.walltime_ms.load(Ordering::Relaxed),
            self.network_bytes.load(Ordering::Relaxed),
            self.dom_nodes.load(Ordering::Relaxed),
            self.js_heap_bytes.load(Ordering::Relaxed),
        )
    }
}

/// Action wrapper used by `check()`. WIT-derived in production; this is
/// the wit-bindgen output shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub action_id: u64,
    pub kind: String,
    pub estimated_walltime_ms: u64,
    pub estimated_net_bytes: u64,
}

/// Kill callback type. Registered by `SessionManager::new` per-session;
/// invoked synchronously by `BudgetEnforcer` on breach.
pub type KillCallback = Arc<dyn Fn(SessionId, KillReason) + Send + Sync>;

/// Per-session state: counters, limits, kill callback, and idempotency flag.
pub struct SessionBudgetEntry {
    pub counters: Arc<SessionCounters>,
    pub limits: BudgetLimits,
    pub kill: KillCallback,
    /// Set to true after the first kill to prevent double invocation.
    pub killed: std::sync::atomic::AtomicBool,
}

impl SessionBudgetEntry {
    pub fn new(counters: Arc<SessionCounters>, limits: BudgetLimits, kill: KillCallback) -> Self {
        Self {
            counters,
            limits,
            kill,
            killed: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

/// Concrete BudgetEnforcer implementation.
pub struct LocalBudgetEnforcer {
    pub(crate) per_session: parking_lot::RwLock<BTreeMap<SessionId, Arc<SessionBudgetEntry>>>,
    pub(crate) obs: Arc<Observability>,
}

impl LocalBudgetEnforcer {
    pub fn new(obs: Arc<Observability>) -> Self {
        Self {
            per_session: parking_lot::RwLock::new(BTreeMap::new()),
            obs,
        }
    }

    /// Register a session's counters + limits + kill-callback.
    /// Called by `SessionManager::create`.
    pub fn register_session(
        &self,
        id: SessionId,
        counters: Arc<SessionCounters>,
        limits: BudgetLimits,
        kill: KillCallback,
    ) {
        let entry = Arc::new(SessionBudgetEntry::new(counters, limits, kill));
        self.per_session.write().insert(id, entry);
    }

    /// Drop session registration on close/abort/kill.
    pub fn unregister_session(&self, id: SessionId) {
        self.per_session.write().remove(&id);
    }
}

/// Public trait surface (per `loom-core_contract.md`).
pub trait BudgetEnforcer: Send + Sync {
    /// Pre-action budget check. Called by `SessionManager` before
    /// dispatching to `loom-host`.
    /// Errors: BudgetExceededWalltime / Network / DomNodes / JsHeap.
    fn check(&self, session: SessionId, action: &Action) -> Result<(), LoomError>;

    /// Post-effect accounting. Atomic `fetch_add(Ordering::Relaxed)`.
    /// On Exceeded, the registered kill-callback fires synchronously.
    /// Errors: BudgetExceeded* (same set as check).
    fn account(&self, session: SessionId, kind: ResourceKind, delta: u64) -> Result<(), LoomError>;

    /// Register a session's counters + limits + kill-callback.
    /// Called by `SessionManager::create`.
    fn register_session(
        &self,
        id: SessionId,
        counters: Arc<SessionCounters>,
        limits: BudgetLimits,
        kill: KillCallback,
    );

    /// Drop session registration on close/abort.
    fn unregister_session(&self, id: SessionId);
}

// BudgetEnforcer impl for LocalBudgetEnforcer lives in impl_local.rs
