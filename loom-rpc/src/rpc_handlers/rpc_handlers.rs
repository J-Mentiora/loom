// RpcHandlers — one handler function per canonical RPC method
// (loom-rpc_contract.md). Each handler routes to either
// `CoreServiceAdapter` (session.* / vault.*) or `HostServiceAdapter`
// (action.<surface>.<verb>) and serialises the result via
// `serde_jcs::to_string` (RFC 8785 canonical JSON).
//
// # Contract semantics
// - **Single dispatch table.** Every method in
//   `loom-rpc_contract.md` maps to one `RpcHandlers::*` async
//   function. The handler set is registered onto a `jsonrpsee::RpcModule`
//   via `RequestRouter::register_methods` at startup.
// - **Routing.** `action.*` handlers call
//   `HostServiceAdapter::dispatch_action`. `session.*` / `vault.*`
//   handlers call `CoreServiceAdapter`. Misrouting will not type-check
//   because the two adapters have incompatible return types.
// - **Canonical JSON.** All response bodies serialised
//   via `serde_jcs::to_string`. Clippy lint `disallowed_methods` bans
//   `serde_json::to_string` outside test code (per the wire-spec's
//   schema-source-of-truth rule).
// - **Errors.** Adapter `LoomError` results are converted via
//   `ErrorTranslator::from_loom_error`. Schema-violation envelopes
//   for vault.grant responses are produced via
//   `SchemaValidator::validate_response` (belt+braces response check).

use crate::core_service_adapter::core_service_adapter::CoreServiceAdapterApi;
pub use crate::error_translator::error_translator::JsonRpcError;
use crate::host_service_adapter::host_service_adapter::HostServiceAdapterApi;
use crate::rpc_observability::rpc_observability::RpcObservabilityApi;
use crate::schema_provider::schema_provider::SchemaProviderApi;
use crate::schema_validator::schema_validator::SchemaValidatorApi;
use std::sync::{Arc, OnceLock};

/// Result type returned by every handler. `Err` carries an already-built
/// JSON-RPC error envelope so the connection-handler layer can encode
/// it directly without re-translating.
pub type HandlerResult<T> = Result<T, JsonRpcError>;

/// Async extension for synchronous shim teardown — the daemon's `session.kill`
/// path needs to await `wasm_host.shutdown_session(...)` with a hard
/// ceiling, but `CoreServiceAdapterApi` and `HostServiceAdapterApi` are
/// both sync. This trait is implemented only by the daemon (via
/// `Arc<loom_host::WasmHost>`-wrapping); test stubs leave it unset on
/// `RpcHandlers` and `session.kill` falls back to the abort-only path.
#[async_trait::async_trait]
pub trait SessionShutdownAsync: Send + Sync {
    /// Drive the host-side shim teardown for `session_id`, completing
    /// within `ceiling_ms` (typically 5000 ms per D12). After the
    /// ceiling, the implementation MUST escalate to SIGKILL and reap.
    async fn shutdown_with_ceiling(&self, session_id: &str, ceiling_ms: u64);
}

/// Per-shim breaker state snapshot, surfaced via `daemon.health`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ShimBreakerSnapshot {
    pub shim_id: String,
    /// `"closed" | "open" | "half-open"` (matches the existing
    /// `BreakerState` enum's debug repr).
    pub state: String,
    pub consecutive_failures: u32,
    pub opened_at_ms: Option<u64>,
}

/// Outcome of probing one shim for deep health. The shim-side probe
/// returns a `ShimHealthInfo` over CBOR; the daemon aggregates that with
/// its own restart bookkeeping into this typed payload.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProbeStatus {
    Ok,
    Timeout,
    Error,
}

/// Per-shim payload returned in `DaemonHealth.deep`. Combines the
/// daemon's view (restart bookkeeping) with the shim's self-report
/// (uptime, requests served).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ShimDeepHealth {
    pub shim_id: String,
    /// Number of times the daemon has respawned this shim (excluding the
    /// initial spawn).
    pub daemon_restart_count: u32,
    /// Epoch ms of the most recent daemon-driven respawn, if any.
    pub daemon_last_restart_at_ms: Option<u64>,
    /// Subprocess-reported milliseconds since its own startup.
    pub shim_uptime_ms: u64,
    /// Subprocess-reported count of non-meta requests served.
    pub shim_requests_served: u64,
    /// Subprocess-reported epoch ms of most recent non-meta request.
    pub shim_last_request_at_ms: Option<u64>,
    pub probe_status: ProbeStatus,
}

/// Wire payload for `daemon.health`. Shallow path is non-blocking and
/// safe to poll frequently; `deep` is populated only when the caller
/// passes `{deep: true}` and the implementation actually supports it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct DaemonHealth {
    pub active_sessions: usize,
    pub shim_breaker_states: Vec<ShimBreakerSnapshot>,
    /// `"enabled"` when LOOM_OTEL_ENABLED + endpoint set; `"disabled"`
    /// otherwise. Cheap snapshot; doesn't probe the OTLP endpoint.
    pub otel_exporter: String,
    /// Populated only when `{deep: true}` was requested AND a
    /// `DaemonHealthAsync` provider is wired. Each entry is the
    /// per-shim probe outcome.
    pub deep: Option<Vec<ShimDeepHealth>>,
}

/// Daemon-health snapshot provider (shallow). Implemented by the daemon
/// (which has `wasm_host.shim_manager` + `core.session_manager` access);
/// test stubs leave it unset and `daemon.health` returns an empty
/// snapshot with a degraded status.
pub trait DaemonHealthProvider: Send + Sync {
    fn snapshot(&self, deep: bool) -> DaemonHealth;
}

/// Async-extension for `daemon.health({deep:true})`. The shim health
/// probe is inherently async (per-shim CBOR round-trip with a 1 s budget,
/// fanned-out concurrently); the sync `DaemonHealthProvider::snapshot`
/// path cannot reach it. Set independently via `set_daemon_health_async`.
///
/// Trade-off: two parallel one-method traits (`SessionShutdownAsync`
/// alongside this one). If a third async escape-hatch lands, refactor
/// into a unified `DaemonAdminAsync` trait — threshold is 3 (see
/// decisions.md #16).
#[async_trait::async_trait]
pub trait DaemonHealthAsync: Send + Sync {
    /// Fan out a `Health` probe to every running shim with a per-shim
    /// timeout (default 1 s, overridable via `LOOM_PROBE_TIMEOUT_MS`).
    /// Returns one `ShimDeepHealth` per shim; non-responsive shims
    /// get `probe_status: Timeout`.
    async fn snapshot_deep(&self) -> Vec<ShimDeepHealth>;
}

/// Bundle of `Arc` handles needed by handlers. Built once at startup
/// and shared via `Arc<RpcHandlers>` across all per-connection tasks.
#[allow(dead_code)]
pub struct RpcHandlers {
    pub(crate) core: Arc<dyn CoreServiceAdapterApi>,
    pub(crate) host: Arc<dyn HostServiceAdapterApi>,
    pub(crate) schemas: Arc<dyn SchemaProviderApi>,
    pub(crate) validator: Arc<dyn SchemaValidatorApi>,
    pub(crate) observability: Arc<dyn RpcObservabilityApi>,
    /// Optional async shim-teardown driver for `session.kill`. Settable
    /// post-construction via `set_session_shutdown` so adding this
    /// capability didn't require changing every `RpcHandlers::new`
    /// caller (~6 test-fixture sites). `None` means `session.kill`
    /// runs as abort-only.
    pub(crate) session_shutdown: OnceLock<Arc<dyn SessionShutdownAsync>>,
    /// Optional health snapshot provider for `daemon.health`. Same
    /// settable-post-construction pattern as `session_shutdown`.
    /// `None` means `daemon.health` returns a degraded empty snapshot.
    pub(crate) health_provider: OnceLock<Arc<dyn DaemonHealthProvider>>,
    /// Optional async provider for the `{deep: true}` probe path. Wired
    /// by the daemon at startup; unset = `daemon.health({deep:true})`
    /// returns shallow with `deep: None`.
    pub(crate) daemon_health_async: OnceLock<Arc<dyn DaemonHealthAsync>>,
}

impl RpcHandlers {
    /// Wire the async shim-teardown driver. Call once at daemon startup
    /// after `RpcHandlers::new`. Returns `false` if already set — that's
    /// a wiring bug, not a recoverable error.
    pub fn set_session_shutdown(&self, shutdown: Arc<dyn SessionShutdownAsync>) -> bool {
        self.session_shutdown.set(shutdown).is_ok()
    }

    /// Wire the health snapshot provider. Called once at daemon startup.
    /// Returns `false` if already set.
    pub fn set_health_provider(&self, provider: Arc<dyn DaemonHealthProvider>) -> bool {
        self.health_provider.set(provider).is_ok()
    }

    /// Wire the async deep-probe provider. Returns `false` if already set.
    pub fn set_daemon_health_async(&self, provider: Arc<dyn DaemonHealthAsync>) -> bool {
        self.daemon_health_async.set(provider).is_ok()
    }
}
