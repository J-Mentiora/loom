//! `loom-daemon` — Loom daemon entry point.
//!
//! Wires `loom-core` + `loom-host` + `loom-rpc` into a running
//! Unix-socket JSON-RPC server. Invoked by `loom serve` .
//!
//! Startup sequence:
//!   1. Parse `--socket` / `--config` args.
//!   2. Construct `CoreApiFacade` (crash-recovery sweep included).
//!   3. Construct `WasmHost` (loads pre-compiled `.cwasm` modules).
//!      On load failure, surfaces return `SurfaceUnavailable` until
//!      `loom postinstall` compiles them.
//!   4. Wire `ConnectionHandlerDeps` (adapters → handlers → router →
//!      auth middleware → schema validator → observability).
//!   5. Bind the Unix socket (`SocketServer::new`).
//!   6. Print `HELLO_TOKEN=<hex>` to stdout .
//!   7. Block on the accept loop until SIGINT / SIGTERM.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;

/// Maximum number of concurrently-active sessions a single daemon will hold.
/// Caps unbounded chromium/context growth (each session spawns a chromium shim
/// plus a `/tmp/loom-chromium-*` profile dir). Overridable via
/// `LOOM_MAX_CONCURRENT_SESSIONS`; default 16. A cap-hit fails fast with
/// `TooManyRequests` (retryable via back-off — reconnecting can't free a slot).
fn max_concurrent_sessions() -> usize {
    static CACHED: OnceLock<usize> = OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("LOOM_MAX_CONCURRENT_SESSIONS")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(16)
    })
}

pub mod reaper;
mod upload_guard;
mod vault_bridge;

use anyhow::{Context, Result};
use loom_core::core_api_facade::{
    CoreApiFacade, CoreConfig, ExportInfo as CoreExportInfo,
    PlaywrightImportResult as CorePlaywrightImportResult,
};
use loom_core::error::LoomError;
use loom_rpc::auth_middleware::auth_middleware::{AuthMiddleware, Token};
use loom_rpc::connection_handler::connection_handler::ConnectionHandlerDeps;
use loom_rpc::core_service_adapter::core_service_adapter::{
    AdapterError, CoreFacadeBridge, CoreServiceAdapter, ExportInfo, GrantInfo, GrantParams,
    PlaywrightImportInfo, VaultAddInfo, VaultAddParams, VaultDeleteSecretInfo,
    VaultDeleteSecretParams, VaultDiagnoseInfo, VaultListLabelsInfo, VaultListLabelsParams,
    VaultSetSecretInfo, VaultSetSecretParams,
};
use loom_rpc::host_service_adapter::host_service_adapter::{
    Action, AdapterError as HostAdapterError, HostServiceAdapter, Receipt, WasmHostBridge,
};
use loom_rpc::request_router::request_router::RequestRouter;
use loom_rpc::rpc_handlers::rpc_handlers::RpcHandlers;
use loom_rpc::rpc_handlers::rpc_handlers::{
    DaemonHealth, DaemonHealthAsync, DaemonHealthProvider, ProbeStatus, SessionShutdownAsync,
    ShimBreakerSnapshot, ShimDeepHealth,
};
use loom_rpc::rpc_observability::rpc_observability::RpcObservability;
use loom_rpc::schema_provider::schema_provider::SchemaProvider;
use loom_rpc::schema_validator::schema_validator::SchemaValidator;
use loom_rpc::socket_server::socket_server::{SocketServer, SocketServerConfig};

// ─── Vault threat-model startup precondition ─────────
//
// The file is embedded at compile time so the daemon binary cannot be built
// without `security/vault_threat_model.md`. At runtime we also require the
// four section headings — together this ensures the runtime
// `threat_model_acknowledged: true` stamp on `vault.grant` is provably
// grounded in a present, well-formed threat-model document.

const VAULT_THREAT_MODEL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../security/vault_threat_model.md"
));

fn check_vault_threat_model() -> Result<()> {
    const REQUIRED_SECTIONS: &[&str] = &[
        "## Attacker Classes",
        "## Security Goals",
        "## Trust Boundaries",
        "## Abuse Cases",
    ];
    if !VAULT_THREAT_MODEL.starts_with("# Vault Threat Model") {
        anyhow::bail!("vault_threat_model.md must start with '# Vault Threat Model'");
    }
    for section in REQUIRED_SECTIONS {
        if !VAULT_THREAT_MODEL.contains(section) {
            anyhow::bail!(
                "vault_threat_model.md missing required section heading: {}",
                section
            );
        }
    }
    Ok(())
}

// ─── Bridge: WasmHost → SessionShutdownAsync ────────────────────────────────
//
// Adapts the daemon's `Arc<loom_host::WasmHost>` to the `SessionShutdownAsync`
// trait that `RpcHandlers::session_kill` calls. Wraps `host.shutdown_session`
// in a `tokio::time::timeout` set to the per-call ceiling so a wedged shim
// can't hold `session.kill` indefinitely — after the ceiling, the inner
// `shutdown_process` has already escalated to SIGKILL via its own
// SIGTERM(2s)→SIGKILL(1s) sequence at process.rs:438-457, so the timeout
// here is a belt-and-braces backstop.

struct WasmHostShutdownAdapter {
    host: Arc<loom_host::WasmHost>,
}

#[async_trait::async_trait]
impl SessionShutdownAsync for WasmHostShutdownAdapter {
    async fn shutdown_with_ceiling(&self, session_id: &str, ceiling_ms: u64) {
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(ceiling_ms),
            self.host.shutdown_session(session_id),
        )
        .await;
    }
}

// ─── DaemonHealthProvider + DaemonHealthAsync ───────────────────────────────
//
// Shallow `DaemonHealthProvider::snapshot` reads `ShimManager.breaker_state_snapshot()`
// (per-shim breaker state) and `core.session_manager.list_sessions_info().len()`
// (active session count). `otel_exporter` reflects the `LOOM_OTEL_ENABLED` env var
// per the existing daemon convention.
//
// Deep `DaemonHealthAsync::snapshot_deep` fans out `ShimRequest::Health` probes
// across every running shim via `ShimManager::probe_health` and aggregates the
// results into typed `ShimDeepHealth` records. Per-shim 1 s budget (env-overridable
// via `LOOM_PROBE_TIMEOUT_MS`), 3 s overall (`LOOM_DEEP_HEALTH_BUDGET_MS`).
// The handler in `loom-rpc::rpc_handlers::mod::daemon_health` selects which path
// to call based on the request's `deep` flag.

struct DaemonHealthBridge {
    core: Arc<CoreApiFacade>,
    wasm_host: Option<Arc<loom_host::WasmHost>>,
}

impl DaemonHealthProvider for DaemonHealthBridge {
    fn snapshot(&self, _deep: bool) -> DaemonHealth {
        let active_sessions = self
            .core
            .list_sessions_info()
            .map(|v| v.iter().filter(|(_, status, _)| status == "active").count())
            .unwrap_or(0);

        let shim_breaker_states = match &self.wasm_host {
            Some(host) => host
                .shim_manager()
                .breaker_state_snapshot()
                .into_iter()
                .map(|(id, state, fails, opened)| ShimBreakerSnapshot {
                    shim_id: id.0,
                    state: format!("{state:?}").to_lowercase(),
                    consecutive_failures: fails as u32,
                    opened_at_ms: opened,
                })
                .collect(),
            None => Vec::new(),
        };

        let otel_exporter = match std::env::var("LOOM_OTEL_ENABLED").ok().as_deref() {
            Some("1") | Some("true") | Some("yes") => "enabled".to_string(),
            _ => "disabled".to_string(),
        };

        // Reaper health counts (best-effort, cheap: a single $TMPDIR readdir + per-dir
        // pidfile stat). Orphan trees = loom-chromium dirs whose session isn't live but whose
        // browser pid is still alive. On any read error these degrade to 0/None, never block.
        let (orphan_browser_trees, oldest_active_session_age_secs) = {
            let live: std::collections::HashSet<String> = self
                .core
                .session_manager
                .live_session_ids()
                .iter()
                .map(|s| s.0.clone())
                .collect();
            let orphans = reaper::proc::scan_browser_dirs(&reaper::proc::browser_tmp_root())
                .into_iter()
                .filter(|e| e.is_dir && !e.is_symlink && !live.contains(&e.session_id))
                .filter(|e| {
                    reaper::proc::read_pidfile(&e.path)
                        .map(reaper::proc::pid_is_alive)
                        .unwrap_or(false)
                })
                .count();
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            (
                orphans,
                self.core.session_manager.oldest_active_age_secs(now_ms),
            )
        };

        DaemonHealth {
            active_sessions,
            shim_breaker_states,
            otel_exporter,
            orphan_browser_trees,
            oldest_active_session_age_secs,
            // Sync `snapshot` cannot reach the async probe. The deep path
            // is populated by `DaemonHealthAsync::snapshot_deep` (see
            // impl below) called from the `daemon_health` handler when
            // `{deep: true}` was requested.
            deep: None,
        }
    }
}

#[async_trait::async_trait]
impl DaemonHealthAsync for DaemonHealthBridge {
    async fn snapshot_deep(&self) -> Vec<ShimDeepHealth> {
        let Some(host) = &self.wasm_host else {
            return Vec::new();
        };
        let manager = host.shim_manager();
        let ids = manager.list_shim_ids();
        // Concurrent fan-out via JoinSet (already in widespread use in
        // loom-host; avoids adding `futures` as a direct dep on loom-daemon).
        // Per-shim timeout is enforced INSIDE `probe_health`/`send_and_await`,
        // never at this level — caller-level cancel would leak
        // `process.pending` (Step 7 finding 5c).
        let mut set: tokio::task::JoinSet<ShimDeepHealth> = tokio::task::JoinSet::new();
        for id in ids {
            let manager = manager.clone();
            set.spawn(async move {
                let state = manager.shim_state(&id);
                let (restart_count, last_restart_at_ms) = state
                    .as_ref()
                    .map(|s| (s.restart_count, s.last_restart_at_ms))
                    .unwrap_or((0, None));
                match manager.probe_health(&id).await {
                    Ok(info) => {
                        // Sec-4 sanity check: warn if shim-reported uptime
                        // exceeds the daemon's view of "time since last
                        // restart" by more than 5 s of clock-skew slack.
                        // Don't synthesize an error — operators should
                        // see the raw data.
                        if let Some(restart_ms) = last_restart_at_ms {
                            let now_ms = now_epoch_ms();
                            let max_plausible =
                                now_ms.saturating_sub(restart_ms).saturating_add(5_000);
                            if info.uptime_ms > max_plausible {
                                tracing::warn!(
                                    shim_id = %id.0,
                                    uptime_ms = info.uptime_ms,
                                    daemon_last_restart_at_ms = restart_ms,
                                    "shim reported uptime exceeds daemon's view of \
                                     time-since-last-restart — possible clock skew or buggy shim"
                                );
                            }
                        }
                        ShimDeepHealth {
                            shim_id: id.0.clone(),
                            daemon_restart_count: restart_count,
                            daemon_last_restart_at_ms: last_restart_at_ms,
                            shim_uptime_ms: info.uptime_ms,
                            shim_requests_served: info.requests_served,
                            shim_last_request_at_ms: info.last_request_at_ms,
                            probe_status: ProbeStatus::Ok,
                        }
                    }
                    Err(e) => {
                        let probe_status =
                            if matches!(e.code, loom_core::error::LoomErrorCode::ShimTimeout) {
                                ProbeStatus::Timeout
                            } else {
                                ProbeStatus::Error
                            };
                        ShimDeepHealth {
                            shim_id: id.0.clone(),
                            daemon_restart_count: restart_count,
                            daemon_last_restart_at_ms: last_restart_at_ms,
                            shim_uptime_ms: 0,
                            shim_requests_served: 0,
                            shim_last_request_at_ms: None,
                            probe_status,
                        }
                    }
                }
            });
        }
        let mut out = Vec::new();
        while let Some(joined) = set.join_next().await {
            if let Ok(item) = joined {
                out.push(item);
            }
        }
        out
    }
}

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// RAII guard that decrements a session's in-flight action counter (and refreshes its idle
/// clock) when an action dispatch returns, no matter which path. Constructed AFTER
/// `Session::action_started`, so creation+drop bracket exactly one action.
struct ActionActivityGuard(Arc<loom_core::session_manager::Session>);

impl Drop for ActionActivityGuard {
    fn drop(&mut self) {
        self.0.action_finished(now_epoch_ms());
    }
}

// ─── Bridge: CoreApiFacade → CoreFacadeBridge ───────────────────────────────

/// Wraps `Arc<CoreApiFacade>` and implements the `CoreFacadeBridge`
/// trait required by `CoreServiceAdapter`. Converts between the
/// `loom-core` and `loom-rpc` type vocabularies.
struct CoreBridge {
    core: Arc<CoreApiFacade>,
    /// Optional WasmHost handle. When present, `close_session_raw` spawns
    /// `host.shutdown_session(...)` into the bridge's `cleanup_tasks`
    /// JoinSet so any session-bound shim subprocesses (e.g. Chromium) get
    /// cooperatively torn down. None when modules haven't been compiled
    /// yet (matches StubHostBridge fallback).
    wasm_host: Option<Arc<loom_host::WasmHost>>,
    /// Tracks background `host.shutdown_session` spawns from
    /// `close_session_raw`. Previously a bare `tokio::spawn` here leaked
    /// `JoinHandle`s — when shim teardown stalled on SIGTERM grace,
    /// each session.close left a never-joined task behind; after a few
    /// sequential sessions the daemon's runtime saturated. Spawns now go
    /// into this `JoinSet`, reaped opportunistically on every close.
    cleanup_tasks: Arc<std::sync::Mutex<tokio::task::JoinSet<()>>>,
}

impl CoreFacadeBridge for CoreBridge {
    fn export_session_to_cas(
        &self,
        session_id: &str,
        format: &str,
    ) -> Result<ExportInfo, AdapterError> {
        let r: CoreExportInfo = self
            .core
            .export_session_to_cas(session_id, format)
            .map_err(|e| map_loom_error(&e))?;
        Ok(ExportInfo {
            session_id: r.session_id,
            format: r.format,
            artifact_ref: r.artifact_ref,
        })
    }

    fn get_export_bytes(&self, artifact_ref: &str) -> Result<Vec<u8>, AdapterError> {
        self.core
            .get_export_bytes(artifact_ref)
            .map_err(|e| map_loom_error(&e))
    }

    fn list_sessions_info(&self) -> Result<Vec<(String, String, u64)>, AdapterError> {
        self.core
            .list_sessions_info()
            .map_err(|e| map_loom_error(&e))
    }

    fn replay_session_to_id(&self, session_id: &str) -> Result<String, AdapterError> {
        self.core
            .replay_session_to_id(session_id)
            .map_err(|e| map_loom_error(&e))
    }

    fn diff_sessions_json(
        &self,
        a: &str,
        b: &str,
        include_screenshots: bool,
    ) -> Result<serde_json::Value, AdapterError> {
        self.core
            .diff_sessions_json(a, b, include_screenshots)
            .map_err(|e| map_loom_error(&e))
    }

    fn inspect_session_json(
        &self,
        session_id: &str,
        at_action: Option<u64>,
    ) -> Result<serde_json::Value, AdapterError> {
        self.core
            .inspect_session_json(session_id, at_action)
            .map_err(|e| map_loom_error(&e))
    }

    fn validate_session_result(
        &self,
        session_id: &str,
    ) -> Result<(bool, Vec<String>), AdapterError> {
        self.core
            .validate_session_result(session_id)
            .map_err(|e| map_loom_error(&e))
    }

    fn import_playwright_from_bytes(
        &self,
        trace_bytes: &[u8],
    ) -> Result<PlaywrightImportInfo, AdapterError> {
        let r: CorePlaywrightImportResult = self
            .core
            .import_playwright_from_bytes(trace_bytes)
            .map_err(|e| map_loom_error(&e))?;
        Ok(PlaywrightImportInfo {
            session_id: r.session_id,
            action_count: r.action_count,
        })
    }

    fn create_session_raw(
        &self,
        profile: &str,
        _network_mode: &str,
        capture_policy: Option<&str>,
        seed: Option<u64>,
        budget: Option<serde_json::Value>,
        no_blocklist: bool,
        no_determinism: bool,
        clock_anchor: Option<u64>,
    ) -> Result<(String, u64), AdapterError> {
        use loom_core::budget_enforcer::BudgetLimits;
        use loom_core::error::LoomErrorCode;
        use loom_core::session_manager::SessionCreateOpts;

        // Concurrency cap: fail fast (retryable) when too many sessions are
        // already active, rather than spawning an unbounded number of chromium
        // shims. Counts the AUTHORITATIVE live-session set (no separate counter
        // to drift or leak — a closed/aborted/crashed session leaves the set, so
        // the count self-reconciles for the common case). The one exception is a
        // corrupt-WAL orphan (torn write from a hard daemon kill): its broken
        // chain can't be terminal-marked, so it would otherwise read as "active"
        // forever. That gap is closed by the startup quarantine sweep, the disk
        // scanner's "corrupt" status (excluded from this count), and the on-demand
        // `session.reap`. Cap-hit → `TooManyRequests` so the caller backs off and
        // retries when a slot frees (NOT a reconnect).
        //
        // This is a best-effort, eventually-consistent resource valve, not a
        // hard security boundary (the daemon is single-user/local): a check-then-
        // create window means N concurrent creates at the boundary can transiently
        // overshoot by up to N-1, but the cap re-reads authoritative state every
        // call so it always self-corrects and can't be defeated long-term. A
        // strict atomic reservation would reintroduce the counter-leak-on-crash
        // problem the live-set design deliberately avoids — not worth it here.
        let cap = max_concurrent_sessions();
        let active = match self.core.list_sessions_info() {
            Ok(v) => v.iter().filter(|(_, status, _)| status == "active").count(),
            Err(e) => {
                // Don't silently disable the cap on a transient read error; log
                // it. Fail-open (allow) is the lesser evil for a local tool —
                // blocking the user on a transient introspection glitch is worse
                // than a momentary overshoot, and the next create re-checks.
                tracing::warn!(
                    metric = "loom_daemon_cap_introspect_error",
                    error = %e,
                    "session cap: could not read active sessions; allowing create"
                );
                0
            }
        };
        if active >= cap {
            tracing::warn!(
                metric = "loom_daemon_cap_reject",
                active,
                cap,
                "session.create rejected: concurrent session cap reached"
            );
            return Err(map_loom_error(&LoomError::new(
                LoomErrorCode::TooManyRequests,
                format!(
                    "concurrent session cap reached ({active}/{cap}); run `loom session reap` \
                     to free leaked slots, or retry after a session closes"
                ),
            )));
        }

        let limits: Option<BudgetLimits> = match budget {
            Some(value) => Some(serde_json::from_value(value).map_err(|e| {
                map_loom_error(&LoomError::new(
                    LoomErrorCode::InvalidArgument,
                    format!("invalid budget JSON: {e}"),
                ))
            })?),
            None => None,
        };
        //  root cause: PRIOR to this fix, the underscore-prefixed
        // `_profile` arg was dropped on the floor here — `--profile safe`
        // validated at the JSON-RPC boundary then never reached the Session.
        // The evaluate gate (B) and download confinement (C) both branch on
        // `Session.profile`, so threading it here is the load-bearing fix.
        let opts = SessionCreateOpts {
            agent_id: "rpc-client".to_string(),
            surface: "web".to_string(),
            seed,
            limits,
            replay_of: None,
            // --clock-anchor pins the session clock via the same override the
            // replay path uses: started_at_ms_override → epoch_ms (impl_local.rs)
            // → CDP initialVirtualTime + Header started_at_ms + replay round-trip.
            // None → epoch falls back to wall-clock now_ms() (unchanged behavior).
            started_at_ms_override: clock_anchor,
            capture_policy: capture_policy.map(|s| s.to_string()),
            no_blocklist,
            no_determinism,
            profile: profile.to_string(),
        };
        if let Some(anchor) = clock_anchor {
            // Greppable signal that a cross-run clock anchor was applied (the
            // session's injected Date.now/performance.now epoch is pinned to this).
            tracing::info!(
                clock_anchor = anchor,
                "session create: clock anchor applied"
            );
        }
        let session_id = self
            .core
            .session_manager
            .create(opts)
            .map_err(|e| map_loom_error(&e))?;
        let created_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        Ok((session_id.0, created_at_ms))
    }

    fn close_session_raw(&self, session_id: &str) -> Result<(), AdapterError> {
        use loom_core::manifest_writer::SessionId;
        let result = self
            .core
            .session_manager
            .close(SessionId(session_id.to_string()))
            .map_err(|e| map_loom_error(&e));

        // Background shim teardown so any session-bound Chromium subprocess
        // gets cooperatively reaped. Tracked in `cleanup_tasks` so the
        // JoinHandle isn't leaked — previously a bare `tokio::spawn` here
        // accumulated never-joined tasks and saturated the daemon runtime
        // after 4-6 sessions. Opportunistic `try_join_next` reap keeps the
        // JoinSet bounded across many close calls.
        if let Some(host) = self.wasm_host.clone() {
            let sid = session_id.to_string();
            // `unwrap` is safe: the only way the mutex is poisoned is if a
            // previous holder panicked while spawning into the JoinSet,
            // which would have already crashed the process — there is no
            // recovery path that's better than propagating the panic.
            let mut set = self.cleanup_tasks.lock().unwrap();
            set.spawn(async move {
                host.shutdown_session(&sid).await;
            });
            // Reap completed cleanups + count for visibility.
            let mut reaped = 0usize;
            while set.try_join_next().is_some() {
                reaped += 1;
            }
            tracing::debug!(
                metric = "loom_daemon_close_cleanup_spawn",
                session_id = %session_id,
                pending = set.len(),
                reaped,
            );
        }

        result
    }

    fn abort_session_raw(&self, session_id: &str, reason: &str) -> Result<(), AdapterError> {
        use loom_core::manifest_writer::SessionId;
        use loom_core::session_manager::AbortReason;
        self.core
            .session_manager
            .abort(
                SessionId(session_id.to_string()),
                AbortReason {
                    reason: reason.to_string(),
                },
            )
            .map_err(|e| map_loom_error(&e))
    }

    // ── Vault bridge methods ────────────────────────────────────────────
    //
    // Each method translates between loom-rpc wire types
    // (`GrantParams`/`VaultAddParams`/`GrantInfo`) and loom-core domain
    // types (`GrantOpts`/`AddCredentialOpts`/`GrantSnapshot`). Errors
    // route through `map_loom_error` so the wire envelope carries the
    // distinct `vault_grant_revoked`/`vault_grant_expired`/`vault_rejection`
    // codes .

    fn vault_grant(&self, p: GrantParams) -> Result<GrantInfo, AdapterError> {
        crate::vault_bridge::vault_grant(&self.core, p)
    }

    fn vault_revoke(&self, grant_id: &str, reason: &str) -> Result<(), AdapterError> {
        crate::vault_bridge::vault_revoke(&self.core, grant_id, reason)
    }

    fn vault_list_grants(&self, session_id: Option<&str>) -> Result<Vec<GrantInfo>, AdapterError> {
        crate::vault_bridge::vault_list_grants(&self.core, session_id)
    }

    fn vault_add(&self, p: VaultAddParams) -> Result<VaultAddInfo, AdapterError> {
        crate::vault_bridge::vault_add(&self.core, p)
    }

    // ── v0.9.4 W6 direct credential bridge methods ──────────────────

    fn vault_set_secret(
        &self,
        p: VaultSetSecretParams,
    ) -> Result<VaultSetSecretInfo, AdapterError> {
        crate::vault_bridge::vault_set_secret(&self.core, p)
    }

    fn vault_delete_secret(
        &self,
        p: VaultDeleteSecretParams,
    ) -> Result<VaultDeleteSecretInfo, AdapterError> {
        crate::vault_bridge::vault_delete_secret(&self.core, p)
    }

    fn vault_list_labels(
        &self,
        p: VaultListLabelsParams,
    ) -> Result<VaultListLabelsInfo, AdapterError> {
        crate::vault_bridge::vault_list_labels(&self.core, p)
    }

    fn vault_get_session_context(
        &self,
    ) -> Result<
        loom_rpc::core_service_adapter::core_service_adapter::VaultGetSessionContextInfo,
        AdapterError,
    > {
        crate::vault_bridge::vault_get_session_context(&self.core)
    }

    fn vault_diagnose(&self) -> Result<VaultDiagnoseInfo, AdapterError> {
        crate::vault_bridge::vault_diagnose(&self.core)
    }

    fn gc_run(
        &self,
        ttl_days: Option<u64>,
        _store_max_bytes: Option<u64>,
    ) -> Result<loom_rpc::core_service_adapter::core_service_adapter::GcRunReport, AdapterError>
    {
        // GC the content store. ttl_days defaults to 7
        // when unset; the AC mandates the default is documented and
        // tested. Honor `store_max_bytes` later via auto-GC threshold
        // (orthogonal to manual `loom gc`).
        let ttl_secs = ttl_days.unwrap_or(7).saturating_mul(24 * 3600);
        let ttl = std::time::Duration::from_secs(ttl_secs);
        let report = self
            .core
            .content_store
            .gc(ttl)
            .map_err(|e| map_loom_error(&e))?;
        Ok(
            loom_rpc::core_service_adapter::core_service_adapter::GcRunReport {
                blobs_scanned: report.blobs_scanned,
                blobs_collected: report.blobs_collected,
                bytes_freed: report.bytes_freed,
            },
        )
    }

    fn session_reap(
        &self,
        dry_run: bool,
    ) -> Result<loom_rpc::core_service_adapter::core_service_adapter::ReapReport, AdapterError>
    {
        // Skip set = sessions live in memory, so a session merely mid-WAL-write
        // is never mistaken for an abandoned corrupt orphan (D4).
        //
        // TOCTOU note: a session created AFTER this snapshot but scanned before
        // it finishes writing is not in `skip`. Two independent guards make a
        // false quarantine effectively impossible: (1) a freshly created session
        // writes a valid Header synchronously in `open_manifest` before
        // `create_session` returns, so `is_corrupt_orphan` sees a well-formed
        // (non-corrupt) WAL and leaves it alone; (2) reap is only needed once
        // corrupt phantoms have saturated the cap — and then `session.create`
        // is already rejected with `TooManyRequests`, so nothing new can race
        // in. Quarantine is non-destructive (dir moved aside, not deleted), so
        // even a pathological miss is recoverable by moving the dir back.
        let skip = self.core.session_manager.live_session_ids();
        let outcome = self
            .core
            .startup_manager
            .quarantine_corrupt_sessions(dry_run, &skip)
            .map_err(|e| map_loom_error(&e))?;

        if !outcome.quarantined.is_empty() || !outcome.failed.is_empty() {
            tracing::warn!(
                metric = "loom_daemon_session_reap",
                dry_run,
                quarantined = outcome.quarantined.len(),
                skipped_live = outcome.skipped_live,
                failed = outcome.failed.len(),
                "session.reap quarantined corrupt orphan session(s)"
            );
        }

        // Also reap leaked LIVE resources: idle/zombie sessions + orphan Chromium trees.
        // `apply = !dry_run` so a dry-run only previews. The sweep is async; run it on the
        // current multi-thread runtime via block_in_place (we're inside a sync adapter call
        // dispatched from the async RPC loop). Best-effort: a sweep failure must not fail the
        // corrupt-WAL reap that already succeeded.
        let sweep = {
            let cfg = reaper::ReaperConfig::from_env();
            let core = Arc::clone(&self.core);
            let host = self.wasm_host.clone();
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async move {
                    reaper::run_sweep(&core, host.as_ref(), &cfg, !dry_run).await
                })
            })
        };

        Ok(
            loom_rpc::core_service_adapter::core_service_adapter::ReapReport {
                quarantined: outcome.quarantined.into_iter().map(|s| s.0).collect(),
                skipped_live: outcome.skipped_live,
                dry_run: outcome.dry_run,
                quarantine_dir: outcome.quarantine_dir.map(|p| p.display().to_string()),
                failed: outcome
                    .failed
                    .into_iter()
                    .map(|f| format!("{}: {}", f.session_id.0, f.details))
                    .collect(),
                idle_evicted: sweep.idle_evicted,
                zombies_closed: sweep.zombies_closed,
                orphan_browsers_killed: sweep.orphan_browsers_killed,
                orphan_dirs_removed: sweep.orphan_dirs_removed,
            },
        )
    }
}

/// Map a `loom-core::LoomError` → `loom-rpc::LoomErrorCode`.
// pub(crate): shared with the vault_bridge submodule (large-file split).
pub(crate) fn map_loom_error(e: &LoomError) -> AdapterError {
    use loom_core::error::LoomErrorCode as CoreCode;
    use loom_rpc::error_translator::error_translator::LoomErrorCode as RpcCode;
    match e.code {
        CoreCode::SessionNotFound | CoreCode::SessionKilled => RpcCode::SessionNotFound,
        CoreCode::SessionAlreadyClosed => RpcCode::SessionClosed,
        CoreCode::SessionAborted => RpcCode::SessionAborted,
        CoreCode::BudgetExceeded | CoreCode::BudgetRateLimited => RpcCode::BudgetExceeded,
        CoreCode::StoreIntegrityFailed | CoreCode::ManifestCorrupt => RpcCode::StoreIntegrityFailed,
        // Distinct kinds: revoke and expire must be
        // distinguishable on the wire. F-A2 / F-S1 / F-S2 fix —
        // previously these collapsed into VaultGrantNotFound.
        //
        // VaultUnknownLabel: keychain has no credential under the
        // requested label. Today (NullKeychain in the daemon's vault
        // wiring) this fires for EVERY vault.grant call until the
        // OAuth device flow lands and populates the keychain via
        // `vault.add`. The wire kind is `vault_grant_not_found` for
        // backward compat, but the structured detail (when surfaced
        // by error_mapper) calls out the missing-credential reason
        // so operators don't chase a phantom grant id.
        CoreCode::VaultUnknownLabel => RpcCode::VaultGrantNotFound,
        CoreCode::VaultGrantRevoked => RpcCode::VaultGrantRevoked,
        CoreCode::VaultGrantExpired => RpcCode::VaultGrantExpired,
        CoreCode::VaultRejection => RpcCode::VaultRejection,
        // Surface trap (genuine wasmtime trap OR guest-returned
        // host-error::shim-failure / store-integrity-failed / etc.
        // that decode_typed_receipt mapped). The rpc-layer
        // LoomErrorCode lacks a dedicated ShimFailure / ShimTimeout
        // variant today, so all shim-derived faults surface as
        // SurfaceTrap; expand this mapping when the rpc enum grows.
        CoreCode::SurfaceTrap
        | CoreCode::ShimFailure
        | CoreCode::ShimTimeout
        | CoreCode::ShimBreakerOpen => RpcCode::SurfaceTrap,
        // profile-restricted is a wire-stable kind
        // that survives daemon → wire translation. Detail (matched_pattern,
        // profile, violation) is currently constructed at the daemon gate
        // site and lives in `Receipt.error.detail`, not in the LoomError
        // context — this arm only matters if a downstream emitter routes
        // ProfileRestricted through `LoomError`.
        CoreCode::ProfileRestricted => RpcCode::ProfileRestricted,
        CoreCode::Unsupported => RpcCode::SurfaceUnavailable,
        // InvalidArgument carries a typed message (e.g. "unsupported
        // export format: cdp"). Map to SchemaViolation on the wire so
        // the receipt's `code` field reflects what's wrong with the
        // request rather than collapsing to the generic `internal_error`.
        // (`InvalidArgument` previously fell into the catchall arm,
        // surfacing as "Error: internal_error: session.export failed
        // for session ..." which gives the operator no actionable
        // signal about what to change.)
        CoreCode::InvalidArgument => RpcCode::SchemaViolation,
        _ => RpcCode::InternalError,
    }
}

// ─── A-W8.1 / W8.5 auth-file permission helpers ────────────────────────────

/// Refuse to start when an existing auth file has loose perms
/// (any of `g+r g+w g+x o+r o+w o+x` set). On a fresh install the file
/// doesn't exist yet → no-op. On Unix only; Windows ACLs are out of
/// scope for v0.9.4.
#[cfg(unix)]
fn probe_auth_perms_or_refuse(path: &std::path::Path, what: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(anyhow::anyhow!(
                "auth-file stat failed for {} at {}: {}",
                what,
                path.display(),
                e
            ));
        }
    };
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        tracing::error!(
            path = %path.display(),
            mode = format!("{:04o}", mode),
            "auth file has loose permissions; refusing to start"
        );
        anyhow::bail!(
            "{} at {} has mode {:04o} (group/world bits set); \
             expected 0600. Run `chmod 600 {}` and restart.",
            what,
            path.display(),
            mode,
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn probe_auth_perms_or_refuse(_path: &std::path::Path, _what: &str) -> Result<()> {
    // Windows ACLs aren't a chmod analogue; v0.9.4 leaves the probe a
    // no-op there. A follow-up that uses `windows::Win32::Security` can
    // implement a similar refuse-on-non-private-DACL check.
    Ok(())
}

/// Tighten a freshly-written auth file to 0600 unconditionally.
/// Unix only; Windows uses ACLs and is out of scope for v0.9.4.
#[cfg(unix)]
fn apply_auth_perms_0600(path: &std::path::Path, what: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).with_context(|| {
        format!(
            "set 0600 on {} at {}; required by the A-W8.1 startup-perms contract",
            what,
            path.display()
        )
    })
}

#[cfg(not(unix))]
fn apply_auth_perms_0600(_path: &std::path::Path, _what: &str) -> Result<()> {
    Ok(())
}

// ─── Vault W6 wire-boundary helpers ────────────────────────────────────────

/// D37 canonical label policy enforced at the wire boundary. The
/// `manifest_writer::append_audit` gate (W5.10 / A-W8.5) catches the
/// same shape as belt-and-suspenders if a future code path bypasses
/// this check.
fn validate_label_canonical(label: &str) -> Result<(), String> {
    if label.is_empty() {
        return Err("label is empty".into());
    }
    if label.len() > 64 {
        return Err(format!("label exceeds 64 chars ({} chars)", label.len()));
    }
    if !label
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == ':' || c == '_' || c == '-')
    {
        return Err(format!(
            "label {label:?} fails canonical validation ^[A-Za-z0-9:_-]{{1,64}}$"
        ));
    }
    Ok(())
}

/// Validate a vault label at the wire boundary and map any rejection to the RPC
/// adapter error (`InvalidArgument`). Shared by `vault_set_secret` /
/// `vault_delete_secret` (D37) so the error mapping lives in one place.
// pub(crate): shared with the vault_bridge submodule (large-file split).
pub(crate) fn validate_label_or_rpc_err(label: &str) -> Result<(), AdapterError> {
    validate_label_canonical(label).map_err(|e| {
        map_loom_error(&LoomError::new(
            loom_core::error::LoomErrorCode::InvalidArgument,
            e,
        ))
    })
}

// ─── Bridge: WasmHost → WasmHostBridge ──────────────────────────────────────

/// Stub host bridge — returns `SurfaceUnavailable` for every action
/// dispatch until WASM modules are compiled by `loom postinstall`.
/// Replaced by a real `WasmHost`-backed impl once modules are present.
struct StubHostBridge;

impl WasmHostBridge for StubHostBridge {
    fn dispatch_action_blocking(&self, _action: Action) -> Result<Receipt, HostAdapterError> {
        use loom_rpc::error_translator::error_translator::LoomErrorCode;
        Err(LoomErrorCode::SurfaceUnavailable)
    }

    // stub bridge means WASM host failed to load (no surfaces).
    // Reporting false here doesn't change behavior — `dispatch_action`
    // already errors with SurfaceUnavailable — but it produces a clearer
    // BrowserNotFound message at session.create when the surfaces dir
    // happens to be empty AND chromium is missing.
    fn has_chromium(&self) -> bool {
        false
    }
}

/// Real bridge wrapping `Arc<loom_host::WasmHost>`. Uses
/// `tokio::task::block_in_place` + `Handle::block_on` so the async
/// dispatch call is safely driven from a sync bridge method.
struct WasmBridge {
    host: Arc<loom_host::WasmHost>,
    core: Arc<CoreApiFacade>,
    /// was a `ShimChromiumConfig` registered at host boot?
    /// Set at `build_host_bridge` time from the resolver's outcome.
    has_chromium: bool,
    /// Allow-list root for `web.set_input_files` (from `LOOM_UPLOAD_ROOT`).
    /// `None` → uploads fail closed. Daemon-global (all sessions), per
    /// plan-council FND#4 — not threaded per-session.
    upload_root: Option<PathBuf>,
}

impl WasmHostBridge for WasmBridge {
    fn dispatch_action_blocking(&self, action: Action) -> Result<Receipt, HostAdapterError> {
        use loom_host::session_executor::{Action as HostAction, ActionOutcome, SessionHandle};
        use loom_rpc::error_translator::error_translator::LoomErrorCode;

        // Owned copy so the borrow doesn't outlive the move-out of
        // `action` further down (v0.9.7 follow-up rewrites action
        // mid-dispatch for cookie grant resolution).
        let session_id_str_owned = action_session_id(&action).to_string();
        let session_id_str = session_id_str_owned.as_str();

        // Resolve the session from core.
        let session = self
            .core
            .session_manager
            .get(loom_core::manifest_writer::SessionId(
                session_id_str.to_string(),
            ))
            .map_err(|_| LoomErrorCode::SessionNotFound)?;

        // Reject terminal sessions before any host dispatch.
        // The check must precede host.dispatch() so the shim is never reached.
        {
            use loom_core::budget_enforcer::KillReason;
            use loom_core::session_manager::SessionStatus;
            let status = *session.status.lock();
            match status {
                SessionStatus::Closed => return Err(LoomErrorCode::SessionClosed),
                SessionStatus::Aborted | SessionStatus::Killed | SessionStatus::Crashed => {
                    // Distinguish budget-driven kills from user/store aborts so
                    // the typed `budget-exceeded` code reaches the wire — the
                    // `kill_reason` was written by the budget kill callback
                    // before status flipped, so we just have to consult it.
                    return Err(match session.kill_reason.lock().as_ref() {
                        Some(KillReason::BudgetExceeded { .. }) => LoomErrorCode::BudgetExceeded,
                        _ => LoomErrorCode::SessionAborted,
                    });
                }
                SessionStatus::Created | SessionStatus::Active => {}
            }
        }

        // Activity tracking for the idle reaper: mark the action in-flight for the WHOLE
        // dispatch (so an actively-working session is never seen as idle, even a single long
        // navigate) and bump `last_activity` on both entry and exit. The guard's Drop runs on
        // every return path — early returns, errors, panics-as-unwind — so `in_flight` can't
        // leak. Placed AFTER the terminal-status check so closed/aborted sessions don't count.
        session.action_started(now_epoch_ms());
        let _activity = ActionActivityGuard(Arc::clone(&session));

        // Safe profile blocks destructive evaluate.
        // Daemon-layer gate (NOT shim) — daemon has typed Action +
        // Session.profile in scope so we can short-circuit before
        // host.dispatch.
        //
        // NOTE: when adding new destructive verbs (e.g. `web.execute_storage_set`),
        // extend this match. The catchall is intentional — non-evaluate
        // actions pass through unchanged, but a future verb that should
        // be safe-gated will silently bypass this block until added here.
        match &action {
            Action::WebEvaluate { expression, .. } if session.profile == "safe" => {
                if let Some(matched) = loom_shared::safety::EVALUATE_DENYLIST
                    .iter()
                    .find(|p| expression.contains(*p))
                {
                    tracing::warn!(
                        session_id = %session_id_str,
                        profile = "safe",
                        matched_pattern = matched,
                        "blocked destructive evaluate"
                    );
                    return Ok(profile_restricted_evaluate_receipt(
                        session.allocate_action_id(),
                        session_id_str,
                        matched,
                    ));
                }
            }
            _ => {}
        }

        // v0.9.7 follow-up A — Grant resolution + Follow-up B — per-cookie
        // validation, both performed daemon-side: under the settled
        // daemon-owns-verbs architecture the WASM guest forwards opaquely and
        // the daemon owns the safety/cookie checks (cookie types live in
        // `loom_shared::cookie_types`). Together they
        // bring the cookie verbs up to the user-facing contract
        // documented in the v0.9.6 task spec:
        //
        //   - `set_cookies` with `CookieSource::Grant` resolves through
        //     `core.vault.substitute_cookies(grant_id, session_id)` and
        //     dispatches with the resolved cookie array as if the
        //     operator had supplied `CookieSource::Inline { cookies }`.
        //   - Per-cookie validation (`validate_cookie_params`: 64-cap,
        //     name/value/expires) runs BEFORE the CDP envelope is
        //     built. Validation failures short-circuit to a typed
        //     `cookie_validation_error` receipt (matching the
        //     existing WASM-side ErrorMapper wire kind and the
        //     action_registry --help docstring) without ever
        //     touching the chromium shim.
        //
        // Shape errors (malformed `source` JSON, missing `cookies` key
        // in the vault blob) flow through the existing
        // `SchemaViolation` / `InternalError` codes — only typed
        // `CookieValidationError` variants flow through the
        // taxonomy-coded `cookie_validation_error` receipt. This split
        // keeps the wire taxonomy a closed set the operator can
        // group dashboards on.
        let action = match action {
            Action::WebSetCookies { session_id, source } => {
                use loom_core::manifest_writer::SessionId;
                use loom_core::vault::GrantId;
                use loom_shared::cookie_types::CookieSource;

                // Step 1: parse the `source` payload into the typed
                // `CookieSource` enum so malformed shapes (missing
                // `source` tag, unknown variants, wrong-typed fields)
                // fail closed via `SchemaViolation` rather than
                // silently falling through to the no-op chromium-args
                // branch with an empty cookies array.
                let typed_source: CookieSource =
                    serde_json::from_value(source).map_err(|_| LoomErrorCode::SchemaViolation)?;

                // Step 2: resolve a `Grant` to its cookies array by
                // calling `Vault::substitute_cookies`. The vault blob
                // schema is the canonical
                // `{"schema_version":1,"cookies":[NetworkCookieParam...]}`
                // produced by `loom vault add --credential-type cookie`;
                // a missing or non-array `cookies` field means the
                // keychain blob is corrupt and we fail closed with
                // `InternalError` rather than silently emitting an
                // empty Network.setCookies envelope.
                let cookies: Vec<loom_shared::cookie_types::NetworkCookieParam> = match typed_source
                {
                    CookieSource::Inline { cookies } => cookies,
                    CookieSource::Grant { grant_id } => {
                        let bytes = self
                            .core
                            .vault
                            .substitute_cookies(GrantId(grant_id), SessionId(session_id.clone()))
                            .map_err(|e| map_loom_error(&e))?;

                        #[derive(serde::Deserialize)]
                        struct VaultCookieBlob {
                            cookies: Vec<loom_shared::cookie_types::NetworkCookieParam>,
                        }

                        serde_json::from_slice::<VaultCookieBlob>(&bytes)
                            .map_err(|e| {
                                tracing::error!(
                                    "vault.substitute_cookies blob deserialise failed: {e}"
                                );
                                LoomErrorCode::InternalError
                            })?
                            .cookies
                    }
                };

                // Step 3: per-cookie validation. Failures short-circuit
                // to a typed `cookie_validation` receipt with the
                // snake_case taxonomy code from `cookie_validation_code`.
                if let Err(e) = loom_shared::cookie_types::validate_cookie_params(&cookies) {
                    return Ok(cookie_validation_error_receipt(
                        session.allocate_action_id(),
                        session_id_str,
                        cookie_validation_code(&e),
                        e.to_string(),
                    ));
                }

                // Step 4: rebuild the action with a uniform `inline`
                // source so the downstream `build_chromium_args` path
                // sees a single shape regardless of how the operator
                // framed the request.
                let resolved_source = serde_json::json!({
                    "source": "inline",
                    "cookies": cookies,
                });
                Action::WebSetCookies {
                    session_id,
                    source: resolved_source,
                }
            }
            // web.set_input_files: AUTHORITATIVE upload allow-list gate
            // (plan-council FND#5 — daemon-side, before the wasm guest /
            // chromium ever see the paths). Enforced in ALL profiles, fail
            // closed when LOOM_UPLOAD_ROOT is unset. On success the action is
            // rebuilt with CANONICALIZED paths so chromium opens the validated
            // file (closing most of the canonicalize→read TOCTOU window).
            Action::WebSetInputFiles {
                session_id,
                selector,
                paths,
            } => {
                match upload_guard::validate_upload_paths(
                    &paths,
                    self.upload_root.as_deref(),
                    upload_guard::MAX_UPLOAD_FILES,
                    upload_guard::MAX_UPLOAD_FILE_BYTES,
                    upload_guard::MAX_UPLOAD_TOTAL_BYTES,
                ) {
                    Ok(canon) => Action::WebSetInputFiles {
                        session_id,
                        selector,
                        paths: canon
                            .into_iter()
                            .map(|p| p.to_string_lossy().into_owned())
                            .collect(),
                    },
                    Err(e) => {
                        tracing::warn!(
                            session_id = %session_id_str,
                            kind = e.kind(),
                            "blocked file upload (upload allow-list)"
                        );
                        return Ok(upload_error_receipt(
                            session.allocate_action_id(),
                            session_id_str,
                            e.kind(),
                            e.message(),
                        ));
                    }
                }
            }
            other => other,
        };

        let handle = tokio::runtime::Handle::current();

        // web.network_log is a host/shim READ — it does NOT run the WASM guest,
        // navigate, or touch the replay hash chain. Intercept here and build the
        // receipt directly from the shim accumulator.
        if let Action::WebNetworkLog { .. } = &action {
            let host = Arc::clone(&self.host);
            let sid = session_id_str.to_string();
            let action_id = session.allocate_action_id();
            let data = tokio::task::block_in_place(|| handle.block_on(host.network_log(&sid)))
                .map_err(|e| {
                    tracing::error!(
                        session_id = %sid,
                        error = %e,
                        "web.network_log failed"
                    );
                    map_loom_error(&e)
                })?;
            return Ok(build_network_log_receipt(action_id, session_id_str, data));
        }

        let session_handle = SessionHandle {
            session_id: session.id.clone(),
            handle: handle.clone(),
            receipt_pool: handle.clone(),
            abort_flag: session.abort_flag.clone(),
            abort_signal: session.abort_notify.clone(),
            kill_reason: session.kill_reason.clone(),
            seed: session.seed,
            epoch_ms: session.epoch_ms,
            no_blocklist: session.no_blocklist,
            // settle-capture (4b): thread the determinism toggle through.
            no_determinism: session.no_determinism,
            // thread profile + downloads_dir into the
            // SessionHandle so HostState can inject env vars at shim spawn.
            profile: session.profile.clone(),
            downloads_dir: session.downloads_dir.clone(),
        };

        // Build the host-side action payload. Three shapes flow through
        // here:
        //
        //   1. `web.navigate` — the WIT guest's `navigate-verb` calls
        //      `host::navigate_execute(&url, deadline_ms)` directly; its
        //      `Action.payload` MUST be raw UTF-8 URL bytes (per the
        //      contract documented at `loom-surface-web::navigate_verb`).
        //      `build_chromium_args` produces a CBOR-shaped CdpMessage,
        //      which `String::from_utf8` rejects — so navigate skips
        //      that path.
        //   2. Other web verbs (click/type/etc.) — route through the
        //      generic `host::shim_call("chromium", &a.payload)` path,
        //      where the chromium shim expects a CBOR-encoded CdpMessage.
        //      `build_chromium_args` produces exactly that.
        //   3. Anything else — fall back to JCS-encoded Action.
        let args_canonical_bytes = match &action {
            // Navigate AND evaluate use the typed host functions
            // (`navigate_execute` / `evaluate_execute`) — both expect
            // raw UTF-8 bytes in the action payload, NOT a CBOR-encoded
            // CdpMessage. Mismatch returns `HostError::Internal("payload
            // not valid UTF-8 ...")` from the guest, which surfaces as
            // `internal_error: action dispatch failed` to the CLI.
            // (This regression was caught after the evaluate-result-not-surfaced
            // feature landed.)
            // settle-capture: the navigate payload is `<until>\n<url>` so the
            // readiness mode rides to the guest (which has no JSON parser and
            // splits once on the newline). URLs are http/https/about:blank —
            // never contain a newline — so the split is unambiguous.
            Action::WebNavigate { url, until, .. } => {
                let mode = until.as_deref().unwrap_or("settled");
                format!("{mode}\n{url}").into_bytes()
            }
            Action::WebEvaluate { expression, .. } => expression.as_bytes().to_vec(),
            // settle-capture: web.wait_for's guest verb (`wait_for_verb`) reads
            // the payload as the readiness mode string and calls the typed
            // `host::wait_for_execute`. Raw UTF-8 `until` bytes (default
            // `settled`) — never a CBOR CdpMessage.
            Action::WebWaitFor { until, .. } => {
                until.as_deref().unwrap_or("settled").as_bytes().to_vec()
            }
            // web.set_input_files: the guest's `set_input_files_verb` decodes
            // {selector, paths} from the payload and calls
            // `host::set_input_files_execute`. Paths here are already
            // canonicalized by the upload gate above. action_hash =
            // sha256(payload) covers selector + canonical paths.
            Action::WebSetInputFiles {
                selector, paths, ..
            } => serde_json::to_vec(&serde_json::json!({
                "selector": selector,
                "paths": paths,
            }))
            .unwrap_or_default(),
            _ => build_chromium_args(&action).unwrap_or_else(|| {
                serde_jcs::to_string(&action)
                    .unwrap_or_default()
                    .into_bytes()
            }),
        };
        // per-session monotonic action_id, allocated at
        // dispatch time. The same id is plumbed through HostState →
        // ReceiptBuilder → ActionReceipt (WAL) → Receipt (RPC reply), so the
        // value the CLI sees matches `loom session inspect` entries[].action_id.
        let host_action = HostAction {
            action_id: session.allocate_action_id(),
            surface: action_surface(&action).to_string(),
            method: action_verb(&action).to_string(),
            args_canonical_bytes,
        };

        let host = Arc::clone(&self.host);
        let outcome = tokio::task::block_in_place(|| {
            handle.block_on(host.dispatch(host_action, session_handle))
        })
        .map_err(|e| {
            // Surface-side dispatch failed at the wasmtime / IPC layer.
            // Don't drop the error message — it's our only signal for
            // diagnosing why navigate / click / evaluate trapped (e.g.
            // shim crash, WIT signature mismatch, OOM in the guest).
            // The ERROR-level log fires regardless of RUST_LOG since the
            // subscriber's default fallback is `warn`. The wire kind
            // stays `SurfaceTrap` so existing CLI / receipt schemas
            // don't churn.
            tracing::error!(
                surface = %action_surface(&action),
                method = %action_verb(&action),
                error = %e,
                "host.dispatch failed → SurfaceTrap"
            );
            LoomErrorCode::SurfaceTrap
        })?;

        match outcome {
            ActionOutcome::Success { builder, .. } => Ok(build_navigate_wire_receipt(
                &builder,
                session_id_str,
                session.capture_policy.as_deref(),
            )),
            ActionOutcome::Aborted { .. } => Err(LoomErrorCode::SessionAborted),
            ActionOutcome::Trapped { loom_error, .. } => {
                // Propagate the typed code the host already mapped.
                // `decode_typed_receipt` translates the WIT
                // `host-error` variant (`shim-failure`,
                // `budget-exceeded`, etc.) into the matching
                // `loom_core::LoomErrorCode`; for genuine wasmtime
                // traps `trap_handler::handle_trap` produces
                // SurfaceTrap. Either way, route through
                // `map_loom_error` so the rpc layer sees the right
                // code instead of a hardcoded SurfaceTrap.
                Err(map_loom_error(&loom_error))
            }
        }
    }

    fn has_chromium(&self) -> bool {
        self.has_chromium
    }
}

/// synthesize an error Receipt for a safe-profile
/// evaluate that matched the denylist. Daemon-layer gate runs BEFORE
/// host.dispatch, so we never touch the shim. The wire shape:
///
/// ```text
/// {
///   "status": "error",
///   "error": {
///     "kind": "profile_restricted",
///     "detail": {
///       "matched_pattern": "<pattern>",
///       "profile": "safe",
///       "violation": "safe_profile_evaluate_denylist_match"
///     }
///   }
/// }
/// ```
///
/// `action_id` comes from `session.allocate_action_id()` so the rejection
/// counts against the per-session monotonic sequence .
fn profile_restricted_evaluate_receipt(
    action_id: u64,
    session_id: &str,
    matched_pattern: &str,
) -> Receipt {
    use loom_rpc::host_service_adapter::host_service_adapter::{ReceiptError, ReceiptStatus};
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Receipt {
        action_id,
        session_id: session_id.to_string(),
        status: ReceiptStatus::Error,
        timing_ticks: 0,
        side_effects: vec![],
        error: Some(ReceiptError {
            kind: "profile_restricted".to_string(),
            detail: Some(serde_json::json!({
                "matched_pattern": matched_pattern,
                "profile": "safe",
                "violation": "safe_profile_evaluate_denylist_match",
            })),
        }),
        action_hash: None,
        outcome_hash: None,
        emitted_at_ms: Some(now),
        url: None,
        final_url: None,
        title: None,
        status_code: None,
        dom_snapshot_hash: None,
        screenshot_after_hash: None,
        console_count: None,
        network_count: None,
        console_lines: vec![],
        network_summary: None,
        network_entries: vec![],
        network_entries_blob_ref: None,
        network_entries_truncated: None,
        settle_until: None,
        settle_outcome: None,
        return_value_json: None,
        return_value_blob_ref: None,
        // v0.9.6 cookie-result fields — not applicable to a
        // profile-restricted evaluate.
        set_cookies_result: None,
        get_cookies_result: None,
        clear_cookies_result: None,
        delete_cookies_result: None,
    }
}

/// v0.9.7 follow-up: build an error Receipt for a per-cookie validation
/// rejection in `web.set_cookies`. The typed `CookieValidationError`
/// taxonomy is surfaced on the receipt's `error.kind` ("cookie_validation_error")
/// and `error.detail.code` (one of `name_empty` / `name_invalid` /
/// `value_too_large` / `too_many_cookies` / `invalid_expires`). The daemon
/// gate is the authoritative emitter under the daemon-owns-verbs architecture
/// (the retired `loom-surfaces` verb-side error mapper is gone).
/// Typed error receipt for `web.set_input_files` allow-list / cap rejections.
/// `kind` is the discrete `UploadError::kind()` wire string (e.g.
/// `upload_path_blocked`) — NOT a `js_throw` JSON blob (plan-council FND#8).
/// `message` uses basenames, not full host paths (L1).
fn upload_error_receipt(action_id: u64, session_id: &str, kind: &str, message: String) -> Receipt {
    use loom_rpc::host_service_adapter::host_service_adapter::{ReceiptError, ReceiptStatus};
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Receipt {
        action_id,
        session_id: session_id.to_string(),
        status: ReceiptStatus::Error,
        timing_ticks: 0,
        side_effects: vec![],
        error: Some(ReceiptError {
            kind: kind.to_string(),
            detail: Some(serde_json::json!({ "message": message })),
        }),
        action_hash: None,
        outcome_hash: None,
        emitted_at_ms: Some(now),
        url: None,
        final_url: None,
        title: None,
        status_code: None,
        dom_snapshot_hash: None,
        screenshot_after_hash: None,
        console_count: None,
        network_count: None,
        console_lines: vec![],
        network_summary: None,
        network_entries: vec![],
        network_entries_blob_ref: None,
        network_entries_truncated: None,
        return_value_json: None,
        return_value_blob_ref: None,
        set_cookies_result: None,
        get_cookies_result: None,
        clear_cookies_result: None,
        delete_cookies_result: None,
        settle_until: None,
        settle_outcome: None,
    }
}

/// Synthesize the `loom.web.network_log` receipt from the host's read of the
/// shim accumulator. Observation-only: no navigate-tier fields, no hash chain.
fn build_network_log_receipt(
    action_id: u64,
    session_id: &str,
    data: loom_host::wasm_host::wasm_host::NetworkLogData,
) -> Receipt {
    use loom_rpc::host_service_adapter::host_service_adapter::ReceiptStatus;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Receipt {
        action_id,
        session_id: session_id.to_string(),
        status: ReceiptStatus::Success,
        timing_ticks: 0,
        side_effects: vec![],
        error: None,
        action_hash: None,
        outcome_hash: None,
        emitted_at_ms: Some(now),
        url: None,
        final_url: None,
        title: None,
        status_code: None,
        dom_snapshot_hash: None,
        screenshot_after_hash: None,
        console_count: None,
        network_count: None,
        console_lines: vec![],
        network_summary: None,
        network_entries: data.network_entries,
        network_entries_blob_ref: data.network_entries_blob_ref,
        network_entries_truncated: Some(data.network_entries_truncated),
        settle_until: None,
        settle_outcome: None,
        return_value_json: None,
        return_value_blob_ref: None,
        set_cookies_result: None,
        get_cookies_result: None,
        clear_cookies_result: None,
        delete_cookies_result: None,
    }
}

fn cookie_validation_error_receipt(
    action_id: u64,
    session_id: &str,
    code: &str,
    message: String,
) -> Receipt {
    use loom_rpc::host_service_adapter::host_service_adapter::{ReceiptError, ReceiptStatus};
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Receipt {
        action_id,
        session_id: session_id.to_string(),
        status: ReceiptStatus::Error,
        timing_ticks: 0,
        side_effects: vec![],
        error: Some(ReceiptError {
            kind: "cookie_validation_error".to_string(),
            detail: Some(serde_json::json!({
                "code": code,
                "message": message,
            })),
        }),
        action_hash: None,
        outcome_hash: None,
        emitted_at_ms: Some(now),
        url: None,
        final_url: None,
        title: None,
        status_code: None,
        dom_snapshot_hash: None,
        screenshot_after_hash: None,
        console_count: None,
        network_count: None,
        console_lines: vec![],
        network_summary: None,
        network_entries: vec![],
        network_entries_blob_ref: None,
        network_entries_truncated: None,
        settle_until: None,
        settle_outcome: None,
        return_value_json: None,
        return_value_blob_ref: None,
        set_cookies_result: None,
        get_cookies_result: None,
        clear_cookies_result: None,
        delete_cookies_result: None,
    }
}

/// v0.9.7 follow-up: map a typed `CookieValidationError` variant to the
/// snake_case error-code string used on the wire error receipt.
fn cookie_validation_code(e: &loom_shared::cookie_types::CookieValidationError) -> &'static str {
    use loom_shared::cookie_types::CookieValidationError as E;
    match e {
        E::NameEmpty => "name_empty",
        E::NameInvalid { .. } => "name_invalid",
        E::ValueTooLarge { .. } => "value_too_large",
        E::InvalidSameSite(_) => "invalid_same_site",
        E::InvalidExpires(_) => "invalid_expires",
        E::TooManyCookies(_) => "too_many_cookies",
    }
}

/// Build a CBOR-encoded `CdpMessage` envelope for the given Web.* action.
/// Returns None for actions that don't have a CDP method mapping yet
/// (caller falls back to the legacy JCS-Action encoding).
///
/// This shape MUST match `loom_shared::shim_protocol::CdpMessage` so
/// `ShimManager::send` can decode the bytes via `ciborium_from_slice`.
/// v0.9.6 helper: convert a `serde_json::Value` (typically a cookie object
/// in `web.set_cookies`'s `source.cookies[]`) into a `ciborium::value::Value`
/// for direct embedding in the CDP CBOR envelope. Returns None for shapes
/// the CDP wire can't represent (e.g. arbitrary nested arrays in places
/// chromium expects scalars) — caller drops the entry rather than the
/// whole batch.
fn serde_json_value_to_cbor(v: serde_json::Value) -> Option<ciborium::value::Value> {
    use ciborium::value::Value;
    use serde_json::Value as J;
    match v {
        J::Null => Some(Value::Null),
        J::Bool(b) => Some(Value::Bool(b)),
        J::String(s) => Some(Value::Text(s)),
        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(Value::Integer(i.into()))
            } else if let Some(u) = n.as_u64() {
                Some(Value::Integer((u as i128).try_into().ok()?))
            } else {
                n.as_f64().map(Value::Float)
            }
        }
        J::Array(arr) => Some(Value::Array(
            arr.into_iter()
                .filter_map(serde_json_value_to_cbor)
                .collect(),
        )),
        J::Object(obj) => Some(Value::Map(
            obj.into_iter()
                .filter_map(|(k, v)| Some((Value::Text(k), serde_json_value_to_cbor(v)?)))
                .collect(),
        )),
    }
}

fn build_chromium_args(action: &Action) -> Option<Vec<u8>> {
    use ciborium::value::Value;

    // Build the `Runtime.evaluate` envelope for selector-driven verbs.
    // `expression` is built from CLI-provided strings (selector / text /
    // value), so we MUST embed them as JSON-encoded string literals
    // (`serde_json::to_string`) — naive `format!("{s}")` would let
    // a `"` in the selector break out of the JS string.
    let runtime_evaluate = |expression: String| -> Value {
        Value::Map(vec![
            (
                Value::Text("method".into()),
                Value::Text("Runtime.evaluate".into()),
            ),
            (
                Value::Text("params".into()),
                Value::Map(vec![
                    (Value::Text("expression".into()), Value::Text(expression)),
                    (Value::Text("returnByValue".into()), Value::Bool(true)),
                    (Value::Text("awaitPromise".into()), Value::Bool(false)),
                ]),
            ),
        ])
    };

    let msg = match action {
        Action::WebNavigate { url, .. } => Value::Map(vec![
            (
                Value::Text("method".into()),
                Value::Text("Page.navigate".into()),
            ),
            (
                Value::Text("params".into()),
                Value::Map(vec![
                    (Value::Text("url".into()), Value::Text(url.clone())),
                    (
                        Value::Text("transitionType".into()),
                        Value::Text("typed".into()),
                    ),
                ]),
            ),
        ]),

        Action::WebClick { selector, .. } => {
            let sel = serde_json::to_string(selector).ok()?;
            runtime_evaluate(format!("document.querySelector({sel}).click()"))
        }

        Action::WebEvaluate { expression, .. } => runtime_evaluate(expression.clone()),

        // web.set_input_files uses the typed `set_input_files_execute` host
        // function (like navigate/evaluate), NOT a single CDP envelope —
        // `args_canonical_bytes` special-cases it before this fn is called.
        // This arm exists only to satisfy the exhaustive match.
        Action::WebSetInputFiles { .. } => return None,
        // Intercepted before build_chromium_args (host/shim read, no CDP).
        Action::WebNetworkLog { .. } => return None,

        Action::WebType { selector, text, .. } => {
            // Direct `el.value = text` bypasses React/Vue/Angular value
            // trackers — the DOM `.value` is set but framework state still
            // thinks the field is empty, so a follow-up form submit fails
            // with "this field is required". Use the native prototype
            // setter so the framework's tracker fires its change observer.
            // (Same approach Playwright/testing-library use for the same
            // reason.)
            let sel = serde_json::to_string(selector).ok()?;
            let val = serde_json::to_string(text).ok()?;
            runtime_evaluate(format!(
                "(function(){{\
                  const el=document.querySelector({sel});\
                  el.focus();\
                  const proto=el.tagName==='TEXTAREA'?HTMLTextAreaElement.prototype:HTMLInputElement.prototype;\
                  const setter=Object.getOwnPropertyDescriptor(proto,'value').set;\
                  setter.call(el,{val});\
                  el.dispatchEvent(new Event('input',{{bubbles:true}}));\
                  el.dispatchEvent(new Event('change',{{bubbles:true}}));\
                }})()"
            ))
        }

        Action::WebSelect {
            selector, value, ..
        } => {
            // Same React/Vue/Angular tracker problem as web.type — the
            // native HTMLSelectElement setter is what frameworks observe.
            let sel = serde_json::to_string(selector).ok()?;
            let val = serde_json::to_string(value).ok()?;
            runtime_evaluate(format!(
                "(function(){{\
                  const el=document.querySelector({sel});\
                  const setter=Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype,'value').set;\
                  setter.call(el,{val});\
                  el.dispatchEvent(new Event('input',{{bubbles:true}}));\
                  el.dispatchEvent(new Event('change',{{bubbles:true}}));\
                }})()"
            ))
        }

        Action::WebHover { selector, .. } => {
            let sel = serde_json::to_string(selector).ok()?;
            runtime_evaluate(format!(
                "document.querySelector({sel}).dispatchEvent(\
                 new MouseEvent('mouseover',{{bubbles:true,cancelable:true}}))"
            ))
        }

        Action::WebScroll {
            selector,
            delta_x,
            delta_y,
            ..
        } => {
            let sel = serde_json::to_string(selector).ok()?;
            let dx = delta_x.unwrap_or(0);
            let dy = delta_y.unwrap_or(0);
            runtime_evaluate(format!(
                "(document.querySelector({sel}) || document.scrollingElement)\
                 .scrollBy({dx}, {dy})"
            ))
        }

        Action::WebWait { selector, .. } => {
            // Single probe — full polling is a follow-up feature.
            let sel = serde_json::to_string(selector).ok()?;
            runtime_evaluate(format!("document.querySelector({sel}) !== null"))
        }

        // settle-capture: web.wait_for uses the typed `wait_for_execute` host
        // function (like navigate/evaluate), NOT a single CDP envelope —
        // `args_canonical_bytes` special-cases it before this fn is called.
        // This arm exists only to satisfy the exhaustive match.
        Action::WebWaitFor { .. } => return None,

        Action::WebScreenshot { .. } => Value::Map(vec![
            (
                Value::Text("method".into()),
                Value::Text("Page.captureScreenshot".into()),
            ),
            (
                Value::Text("params".into()),
                Value::Map(vec![(
                    Value::Text("format".into()),
                    Value::Text("png".into()),
                )]),
            ),
        ]),

        Action::WebSnapshot { .. } => Value::Map(vec![
            (
                Value::Text("method".into()),
                Value::Text("DOM.getDocument".into()),
            ),
            (
                Value::Text("params".into()),
                Value::Map(vec![
                    (
                        Value::Text("depth".into()),
                        Value::Integer((-1i128).try_into().ok()?),
                    ),
                    // pierce:true inlines shadow-DOM + iframe contentDocument subtrees,
                    // matching web.navigate (shim STEP 5) so the two DOM captures hash a
                    // comparable node set. Normalized via dom_normalize (frameId stripped
                    // recursively) at the shim cdp_send chokepoint.
                    (Value::Text("pierce".into()), Value::Bool(true)),
                ]),
            ),
        ]),

        // v0.9.6 web-cookie-injection: build CDP envelopes daemon-side
        // (the WASM verbs in loom-surface-web forward whatever
        // action.payload they receive via host::shim_call, so we need
        // the payload to be a valid CDP CBOR envelope by the time it
        // reaches the chromium shim).
        //
        // Per-cookie validation (validate_cookie_params) is intentionally
        // NOT performed on this raw-JSON `source` path — it would require
        // converting the untyped `source` into typed `loom_shared::cookie_types`
        // structs first. The inline path validates via the typed
        // `validate_cookie_params` above; on this path the chromium shim's
        // Network.setCookies response surfaces individual cookie rejections.
        //
        // Grant resolution is now performed upstream in
        // `dispatch_action_blocking` (v0.9.7 follow-up A) — by the
        // time we get here the source should always be `inline`.
        // The non-inline branch below is defensive: if some other
        // caller (e.g. tests) hands us a `grant` source we emit an
        // empty no-op envelope rather than trapping.
        Action::WebSetCookies { source, .. } => {
            // source = {"source":"inline","cookies":[...]} or
            // {"source":"grant","grant_id":"..."}
            let kind = source.get("source").and_then(|v| v.as_str())?;
            if kind != "inline" {
                tracing::warn!(
                    "build_chromium_args saw set_cookies with non-inline source after dispatcher should have resolved it; emitting empty Network.setCookies",
                );
                return Some({
                    let v = Value::Map(vec![
                        (
                            Value::Text("method".into()),
                            Value::Text("Network.setCookies".into()),
                        ),
                        (
                            Value::Text("params".into()),
                            Value::Map(vec![(Value::Text("cookies".into()), Value::Array(vec![]))]),
                        ),
                    ]);
                    let mut bytes = Vec::new();
                    ciborium::ser::into_writer(&v, &mut bytes).ok()?;
                    bytes
                });
            }
            let cookies = source
                .get("cookies")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|c| serde_json_value_to_cbor(c.clone()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Value::Map(vec![
                (
                    Value::Text("method".into()),
                    Value::Text("Network.setCookies".into()),
                ),
                (
                    Value::Text("params".into()),
                    Value::Map(vec![(Value::Text("cookies".into()), Value::Array(cookies))]),
                ),
            ])
        }
        Action::WebGetCookies { urls, .. } => {
            let mut params: Vec<(Value, Value)> = vec![];
            if let Some(u) = urls {
                params.push((
                    Value::Text("urls".into()),
                    Value::Array(u.iter().map(|s| Value::Text(s.clone())).collect()),
                ));
            }
            Value::Map(vec![
                (
                    Value::Text("method".into()),
                    Value::Text("Network.getCookies".into()),
                ),
                (Value::Text("params".into()), Value::Map(params)),
            ])
        }
        Action::WebClearCookies { .. } => Value::Map(vec![
            (
                Value::Text("method".into()),
                Value::Text("Network.clearBrowserCookies".into()),
            ),
            (Value::Text("params".into()), Value::Map(vec![])),
        ]),
        Action::WebDeleteCookies {
            name,
            url,
            domain,
            path,
            ..
        } => {
            let mut params: Vec<(Value, Value)> =
                vec![(Value::Text("name".into()), Value::Text(name.clone()))];
            if let Some(u) = url {
                params.push((Value::Text("url".into()), Value::Text(u.clone())));
            }
            if let Some(d) = domain {
                params.push((Value::Text("domain".into()), Value::Text(d.clone())));
            }
            if let Some(p) = path {
                params.push((Value::Text("path".into()), Value::Text(p.clone())));
            }
            Value::Map(vec![
                (
                    Value::Text("method".into()),
                    Value::Text("Network.deleteCookies".into()),
                ),
                (Value::Text("params".into()), Value::Map(params)),
            ])
        }
    };

    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&msg, &mut bytes).ok()?;
    Some(bytes)
}

/// Construct the wire `Receipt` for a successful action outcome.
///
/// Decodes the three navigate JSON blobs (`navigate_*_json`) into
/// typed wire fields; degrades to empty / None with `tracing::warn` on
/// decode failure — observability fields shouldn't fail the navigate.
/// Applies `apply_capture_profile_to_wire` last so `--capture-policy
/// minimal` strips tier-2 fields per.
fn build_navigate_wire_receipt(
    builder: &loom_host::receipt_marshaller::ReceiptBuilder,
    session_id: &str,
    capture_policy_str: Option<&str>,
) -> Receipt {
    use loom_host::receipt_marshaller::ReceiptStatus as HostStatus;
    use loom_rpc::host_service_adapter::host_service_adapter::ReceiptStatus;

    let status = match builder.status {
        HostStatus::Ok => ReceiptStatus::Success,
        _ => ReceiptStatus::Error,
    };
    let action_hash = (!builder.action_hash.is_empty()).then(|| builder.action_hash.clone());
    let outcome_hash = (!builder.outcome_hash.is_empty()).then(|| builder.outcome_hash.clone());
    let emitted_at_ms = (builder.emitted_at_ms != 0).then_some(builder.emitted_at_ms);

    // decode shim-captured network events from the
    // WIT side-effects-json escape hatch onto the wire receipt's typed
    // `side_effects[]` array.
    let side_effects: Vec<serde_json::Value> = builder
        .navigate_side_effects_json
        .as_deref()
        .map(|bytes| {
            match serde_json::from_slice::<Vec<loom_shared::navigate_outcome::LoomNetworkEvent>>(
                bytes,
            ) {
                Ok(events) => events
                    .into_iter()
                    .filter_map(|e| serde_json::to_value(&e).ok())
                    .collect(),
                Err(e) => {
                    tracing::warn!(
                        action_id = builder.action_id,
                        error = %e,
                        "navigate receipt: side_effects decode failed; emitting empty"
                    );
                    Vec::new()
                }
            }
        })
        .unwrap_or_default();

    // console_lines verbatim.
    let console_lines: Vec<loom_shared::navigate_outcome::ShimConsoleLine> = builder
        .navigate_console_lines_json
        .as_deref()
        .map(|bytes| match serde_json::from_slice(bytes) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    action_id = builder.action_id,
                    error = %e,
                    "navigate receipt: console_lines decode failed; emitting empty"
                );
                Vec::new()
            }
        })
        .unwrap_or_default();

    // typed NetworkSummary aggregate.
    let network_summary: Option<loom_core::receipt_builder::receipt_builder::NetworkSummary> =
        builder
            .navigate_network_summary_json
            .as_deref()
            .and_then(|bytes| match serde_json::from_slice(bytes) {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!(
                        action_id = builder.action_id,
                        error = %e,
                        "navigate receipt: network_summary decode failed; emitting None"
                    );
                    None
                }
            });

    // surface the evaluate return value on the
    // wire. The host's `evaluate_execute` populates
    // `evaluate_return_value_json` (inline-sized) OR
    // `evaluate_return_value_blob_ref` (offloaded to content store);
    // for non-evaluate actions both are `None` and the fields are
    // skipped on serialisation. `blob_ref.sha256` is the wire form so
    // CLI consumers can fetch via `loom content get`.
    let return_value_json = builder.evaluate_return_value_json.clone();
    let return_value_blob_ref = builder
        .evaluate_return_value_blob_ref
        .as_ref()
        .map(|cref| cref.sha256.clone());

    // Observational network-entries side-channel (NOT hash-chained). Decode the
    // inline JSON bytes into `Vec<Value>`; when offloaded, the bytes are absent
    // and `network_entries_blob_ref` carries the sha256 instead.
    let network_entries: Vec<serde_json::Value> = builder
        .navigate_network_entries_json
        .as_deref()
        .map(|bytes| {
            match serde_json::from_slice::<Vec<loom_shared::navigate_outcome::LoomNetworkEntry>>(
                bytes,
            ) {
                Ok(entries) => entries
                    .into_iter()
                    .filter_map(|e| serde_json::to_value(&e).ok())
                    .collect(),
                Err(e) => {
                    tracing::warn!(
                        action_id = builder.action_id,
                        error = %e,
                        "navigate receipt: network_entries decode failed; emitting empty"
                    );
                    Vec::new()
                }
            }
        })
        .unwrap_or_default();
    let network_entries_blob_ref = builder
        .navigate_network_entries_blob_ref
        .as_ref()
        .map(|cref| cref.sha256.clone());
    let network_entries_truncated = builder.navigate_network_entries_truncated;

    let mut receipt = Receipt {
        action_id: builder.action_id,
        session_id: session_id.to_string(),
        status,
        timing_ticks: builder.finished_at_ms.saturating_sub(builder.started_at_ms),
        side_effects,
        error: builder
            .error_code
            .as_ref()
            .map(|c| build_wire_receipt_error(c, builder.error_details.as_deref())),
        action_hash,
        outcome_hash,
        emitted_at_ms,
        url: builder.navigate_url.clone(),
        final_url: builder.navigate_final_url.clone(),
        title: builder.navigate_title.clone(),
        status_code: builder.navigate_status_code,
        dom_snapshot_hash: builder.navigate_dom_snapshot_hash.clone(),
        screenshot_after_hash: builder.navigate_screenshot_after_hash.clone(),
        console_count: builder.navigate_console_count,
        network_count: builder.navigate_network_count,
        console_lines,
        network_summary,
        network_entries,
        network_entries_blob_ref,
        network_entries_truncated,
        // settle-capture readiness fields (surfaced from the builder).
        settle_until: builder.navigate_settle_until.clone(),
        settle_outcome: builder.navigate_settle_outcome.clone(),
        return_value_json,
        return_value_blob_ref,
        // v0.9.6 cookie-result wire fields. Populated by Tier 4
        // (ReceiptMarshaller cookie fields + D13 tuple-identity sort);
        // for now stay `None` so non-cookie verbs serialise unchanged
        // and cookie verbs' result data lands on the receipt once the
        // marshaller exposes it.
        set_cookies_result: None,
        get_cookies_result: None,
        clear_cookies_result: None,
        delete_cookies_result: None,
    };

    // apply per-session capture-policy at the wire
    // boundary. Unknown / unset values → CaptureProfile::Default (no-op).
    let profile = capture_policy_str
        .and_then(loom_core::receipt_builder::receipt_builder::capture_profile_from_str)
        .unwrap_or(loom_core::receipt_builder::receipt_builder::CaptureProfile::Default);
    loom_rpc::host_service_adapter::wire_capture::apply_capture_profile_to_wire(
        &mut receipt,
        profile,
    );
    tracing::debug!(
        action_id = receipt.action_id,
        ?profile,
        "navigate receipt: capture-policy applied"
    );
    receipt
}

/// Build the wire `ReceiptError` from the host-side `ReceiptBuilder`'s
/// `error_code` + `error_details`. Two shapes feed in:
///
/// 1. **Typed shim failure** — `error_code = "shim-failure"`,
///    `error_details = JSON {"kind": "...", "url": "...", ...}`.
///    Hoists the `kind` field to the wire `ReceiptError.kind` and puts
///    the remaining fields into `detail`.
///
/// 2. **Untyped shim failure or other host error** — kind defaults to
///    the host's `error_code`; `detail` wraps the raw `error_details`
///    string in `{"message": "..."}` (or is omitted when empty).
fn build_wire_receipt_error(
    error_code: &str,
    error_details: Option<&str>,
) -> loom_rpc::host_service_adapter::host_service_adapter::ReceiptError {
    use loom_rpc::host_service_adapter::host_service_adapter::ReceiptError;

    if error_code == "shim-failure" {
        if let Some(detail_str) = error_details {
            if let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(detail_str) {
                if let Some(kind) = parsed
                    .get("kind")
                    .and_then(|k| k.as_str())
                    .map(String::from)
                {
                    if let Some(obj) = parsed.as_object_mut() {
                        obj.remove("kind");
                    }
                    let detail = match parsed.as_object() {
                        Some(map) if map.is_empty() => None,
                        _ => Some(parsed),
                    };
                    return ReceiptError { kind, detail };
                }
            }
        }
    }
    let detail = error_details
        .filter(|s| !s.is_empty())
        .map(|s| serde_json::json!({ "message": s }));
    ReceiptError {
        kind: error_code.to_string(),
        detail,
    }
}

fn action_session_id(action: &Action) -> &str {
    match action {
        Action::WebNavigate { session_id, .. }
        | Action::WebClick { session_id, .. }
        | Action::WebEvaluate { session_id, .. }
        | Action::WebType { session_id, .. }
        | Action::WebScreenshot { session_id, .. }
        | Action::WebSelect { session_id, .. }
        | Action::WebHover { session_id, .. }
        | Action::WebScroll { session_id, .. }
        | Action::WebWait { session_id, .. }
        | Action::WebSnapshot { session_id } => session_id,
        Action::WebWaitFor { session_id, .. } => session_id,
        Action::WebSetInputFiles { session_id, .. } => session_id,
        // v0.9.6 web-cookie-injection.
        Action::WebSetCookies { session_id, .. }
        | Action::WebGetCookies { session_id, .. }
        | Action::WebClearCookies { session_id }
        | Action::WebDeleteCookies { session_id, .. } => session_id,
        Action::WebNetworkLog { session_id } => session_id,
    }
}

fn action_surface(_action: &Action) -> &str {
    // Must match the file-stem used by `ModuleLibrary::load_all`
    // (loom-host/src/module_library/interfaces.rs:80) which keys
    // surfaces by the .cwasm file stem. `loom postinstall` produces
    // `loom_surface_web.cwasm`, so the lookup is `SurfaceName("loom_surface_web")`.
    "loom_surface_web"
}

fn action_verb(action: &Action) -> &str {
    // Must match the WIT export name in `wit/loom-surface.wit` verbatim.
    // `web.type-text` and the v0.9.6 cookie verbs (`set-cookies`,
    // `get-cookies`, `clear-cookies`, `delete-cookies`) are the
    // kebab-cased verbs.
    match action {
        Action::WebNavigate { .. } => "navigate",
        Action::WebClick { .. } => "click",
        Action::WebEvaluate { .. } => "evaluate",
        Action::WebType { .. } => "type-text",
        Action::WebScreenshot { .. } => "screenshot",
        Action::WebSelect { .. } => "select",
        Action::WebHover { .. } => "hover",
        Action::WebScroll { .. } => "scroll",
        Action::WebWait { .. } => "wait",
        Action::WebWaitFor { .. } => "wait-for",
        Action::WebSnapshot { .. } => "snapshot",
        Action::WebSetInputFiles { .. } => "set-input-files",
        // v0.9.6 web-cookie-injection.
        Action::WebSetCookies { .. } => "set-cookies",
        Action::WebGetCookies { .. } => "get-cookies",
        Action::WebClearCookies { .. } => "clear-cookies",
        Action::WebDeleteCookies { .. } => "delete-cookies",
        Action::WebNetworkLog { .. } => "network-log",
    }
}

// ─── Daemon startup arguments ────────────────────────────────────────────────

#[derive(Debug)]
struct DaemonArgs {
    socket_path: PathBuf,
    data_root: PathBuf,
    log_path: PathBuf,
    otel_enabled: bool,
    default_seed: u64,
    checkpoint_every_n: u64,
    /// Allow-list root for `web.set_input_files` (`LOOM_UPLOAD_ROOT`).
    /// `None` → file uploads fail closed (deny all).
    upload_root: Option<PathBuf>,
}

impl Default for DaemonArgs {
    fn default() -> Self {
        let data_root = data_root_default();
        let log_path = data_root.join("daemon.log");
        Self {
            socket_path: SocketServer::default_socket_path(),
            data_root,
            log_path,
            otel_enabled: false,
            default_seed: 0,
            checkpoint_every_n: 100,
            upload_root: None,
        }
    }
}

fn data_root_default() -> PathBuf {
    // Per the wire-spec's data-dir conventions: macOS uses ~/Library/Application Support/loom; Linux uses $XDG_DATA_HOME/loom.
    #[cfg(target_os = "macos")]
    {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("loom")
    }
    #[cfg(not(target_os = "macos"))]
    {
        dirs::data_dir()
            .or_else(|| std::env::var("XDG_DATA_HOME").ok().map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("loom")
    }
}

/// Parse CLI args. Minimal: `--socket PATH` and `--config PATH` (config
/// parsing not yet implemented — uses env vars + defaults).
fn parse_args(argv: &[String]) -> DaemonArgs {
    let mut args = DaemonArgs::default();

    // Override from env vars (precedence: CLI > env > config > defaults).
    if let Ok(v) = std::env::var("LOOM_SOCKET_PATH") {
        args.socket_path = PathBuf::from(v);
    }
    if let Ok(v) = std::env::var("LOOM_DATA_ROOT") {
        let root = PathBuf::from(&v);
        args.log_path = root.join("daemon.log");
        args.data_root = root;
    }
    if let Ok(v) = std::env::var("LOOM_LOG_PATH") {
        args.log_path = PathBuf::from(v);
    }
    if std::env::var("LOOM_OTEL_ENABLED").as_deref() == Ok("1") {
        args.otel_enabled = true;
    }
    // Upload allow-list root for web.set_input_files. Unset → uploads fail
    // closed (deny all). Enforced in ALL profiles, daemon-global.
    if let Ok(v) = std::env::var("LOOM_UPLOAD_ROOT") {
        if !v.is_empty() {
            args.upload_root = Some(PathBuf::from(v));
        }
    }

    // Override socket from `--socket PATH` flag.
    let mut iter = argv.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--socket" {
            if let Some(path) = iter.next() {
                args.socket_path = PathBuf::from(path);
            }
        }
        if arg == "--data-root" {
            if let Some(path) = iter.next() {
                let root = PathBuf::from(path);
                args.log_path = root.join("daemon.log");
                args.data_root = root;
            }
        }
    }

    args
}

/// `loom-daemon --help` short-circuit. Mirrors the flags `parse_args`
/// recognises so users can discover them without grepping the source.
fn print_daemon_help() {
    println!(
        "loom-daemon — long-lived RPC server backing the loom CLI / SDKs.\n\
         \n\
         Usually spawned by `loom serve`. Direct invocation is supported but\n\
         rare; the CLI handles socket path + lifetime management for you.\n\
         \n\
         USAGE:\n    \
             loom-daemon [OPTIONS]\n\
         \n\
         OPTIONS:\n    \
             --socket <PATH>      Override the Unix socket path.\n    \
             --data-root <PATH>   Override the data-root directory (sessions, CAS, logs).\n    \
             -h, --help           Print this help and exit.\n    \
             -V, --version        Print version and exit.\n\
         \n\
         ENVIRONMENT:\n    \
             LOOM_SOCKET_PATH     Same as --socket.\n    \
             LOOM_DATA_ROOT       Same as --data-root.\n    \
             LOOM_LOG_PATH        Override the daemon log file path.\n    \
             LOOM_OTEL_ENABLED    Set to `1` to enable OTEL exports.\n    \
             LOOM_UPLOAD_ROOT     Allow-list root for web.set_input_files. Unset → uploads fail closed.\n    \
             LOOM_MAX_CONCURRENT_SESSIONS  Hard cap on concurrent sessions (default 16).\n    \
             LOOM_SESSION_IDLE_TTL_SECS    Evict sessions idle this long (default 1800; 0 disables).\n    \
             LOOM_REAPER_SWEEP_SECS        Reaper sweep cadence (default 60).\n    \
             LOOM_REAP_KILL_GRACE_MS       SIGTERM→SIGKILL grace per orphan tree (default 2000).\n    \
             LOOM_REAP_ORPHAN_MIN_AGE_SECS Min age before an orphan dir is GC'd (default 60).\n    \
             LOOM_REAPER_ORPHAN_GC         Orphan-Chromium GC on/off (default on; set 0 to disable).\n"
    );
}

// ─── Public entry point ──────────────────────────────────────────────────────
//
// exposed as `pub fn run()` so the `loom-daemon` binary can live
// in `loom-cli/src/bin/loom-daemon.rs` (a thin shim) and cargo-dist 0.30+
// ships all 4 loom binaries from one Cargo Package in one tarball — its docs
// require all `[[bin]]` entries to be in one Package to bundle.

pub fn run() -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;
    rt.block_on(async_main())
}

async fn async_main() -> Result<()> {
    let argv: Vec<String> = std::env::args().collect();

    // Short-circuit on --help / --version BEFORE the vault check + socket
    // bind. Otherwise a user typing `loom-daemon --help` either spawns a
    // long-lived daemon (no daemon already running) or fails opaquely with
    // `AddressInUse` (one is). Neither is what --help should do.
    if argv.iter().any(|a| a == "--help" || a == "-h") {
        print_daemon_help();
        return Ok(());
    }
    if argv.iter().any(|a| a == "--version" || a == "-V") {
        println!("loom-daemon {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    //.1 startup gate (F-S6): refuse to start without a
    // present, well-formed threat-model document.
    check_vault_threat_model().context("vault threat-model precondition failed")?;

    let args = parse_args(&argv);

    // Init tracing to stderr so stdout stays clean for HELLO_TOKEN.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    // Ensure data directories exist.
    std::fs::create_dir_all(&args.data_root)
        .with_context(|| format!("create data_root {}", args.data_root.display()))?;
    if let Some(parent) = args.log_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    if let Some(parent) = args.socket_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create socket dir {}", parent.display()))?;
    }

    // 1a. Resolve the keychain backend per LOOM_KEYCHAIN_BACKEND +
    //     LOOM_KEYCHAIN_ALLOW_PROMPT. When an OS-backed backend is
    //     explicitly requested (`macos` | `linux` | `auto`), init failure
    //     is hard-fail-closed — no silent fallback to a stub (per D7).
    //     When the env var is UNSET, default to `in_memory` so the
    //     daemon starts in CI / dev-test contexts that don't have a
    //     keychain daemon running. Production deployments must opt in
    //     explicitly via `LOOM_KEYCHAIN_BACKEND=auto` (or =macos / =linux).
    let keychain_cfg = {
        use std::io::IsTerminal;
        let backend = match std::env::var("LOOM_KEYCHAIN_BACKEND").ok().as_deref() {
            Some("stub") => loom_keychain::BackendChoice::Stub,
            Some("in_memory") => loom_keychain::BackendChoice::InMemory,
            Some("macos") => loom_keychain::BackendChoice::MacOs,
            Some("linux") => loom_keychain::BackendChoice::Linux,
            Some("auto") => loom_keychain::KeychainConfig::default().backend,
            Some(other) => {
                anyhow::bail!(
                    "loom-daemon: unknown LOOM_KEYCHAIN_BACKEND={other}; \
                     expected one of: stub | in_memory | macos | linux | auto"
                );
            }
            None => loom_keychain::BackendChoice::InMemory,
        };
        let allow_prompt = match std::env::var("LOOM_KEYCHAIN_ALLOW_PROMPT").ok().as_deref() {
            Some("0") | Some("false") => false,
            Some("1") | Some("true") => true,
            Some(other) => {
                anyhow::bail!(
                    "loom-daemon: invalid LOOM_KEYCHAIN_ALLOW_PROMPT={other}; expected 0|1"
                );
            }
            None => std::io::stdin().is_terminal() && std::io::stderr().is_terminal(),
        };
        loom_keychain::KeychainConfig {
            backend,
            allow_prompt,
            service_id: "loom",
        }
    };
    let keychain = match loom_keychain::select_keychain(&keychain_cfg) {
        Ok(kc) => {
            tracing::info!(
                backend = ?keychain_cfg.backend,
                service_id = keychain_cfg.service_id,
                allow_prompt = keychain_cfg.allow_prompt,
                "loom-daemon: keychain backend initialised"
            );
            kc
        }
        Err(e) => {
            tracing::error!(
                backend = ?keychain_cfg.backend,
                error = %e,
                "loom-daemon: keychain backend failed to initialise; refusing to start"
            );
            anyhow::bail!(
                "loom-daemon: {:?} keychain backend failed to initialise: {}. \
                 Set LOOM_KEYCHAIN_BACKEND=stub to run without keychain persistence \
                 (NOT recommended for production).",
                keychain_cfg.backend,
                e
            );
        }
    };

    // 1b. Build CoreApiFacade with the resolved keychain.
    let core_config = CoreConfig {
        data_root: args.data_root.clone(),
        log_path: args.log_path.clone(),
        otel_enabled: args.otel_enabled,
        default_seed: args.default_seed,
        checkpoint_every_n: args.checkpoint_every_n,
    };
    let core = CoreApiFacade::new(core_config, keychain).context("CoreApiFacade::new failed")?;

    // 2. Crash-recovery sweep. Recovery errors are non-fatal — the daemon
    //    continues serving — but the report is logged (not discarded) so an
    //    operator can see crashed/quarantined counts at startup.
    match core.startup_manager.perform_recovery_sweep() {
        Ok(report) => {
            if report.sessions_crashed > 0
                || report.sessions_quarantined > 0
                || !report.failed_sessions.is_empty()
                || report.orphan_tmpfiles_removed > 0
            {
                tracing::warn!(
                    metric = "loom_daemon_recovery_sweep",
                    sessions_recovered = report.sessions_recovered,
                    sessions_crashed = report.sessions_crashed,
                    sessions_quarantined = report.sessions_quarantined,
                    orphan_tmpfiles_removed = report.orphan_tmpfiles_removed,
                    failed_sessions = report.failed_sessions.len(),
                    "startup crash-recovery sweep completed"
                );
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "startup crash-recovery sweep failed (non-fatal)");
        }
    }

    // 3. Build WasmHost (or stub if surfaces aren't compiled yet).
    let (host_bridge, wasm_host_handle): (Arc<dyn WasmHostBridge>, _) =
        build_host_bridge(Arc::clone(&core), args.upload_root.clone());

    // 3b. Startup orphan-Chromium GC. A previous daemon's unclean exit can leave
    //     `loom-chromium-*` user-data-dirs whose sessions are gone but whose browser trees
    //     still hold fds/pids. The live set is empty here (no sessions created yet), so every
    //     aged loom-chromium dir is an orphan — reap it before serving so a churned host
    //     starts clean. Best-effort; never fatal.
    {
        let reaper_cfg = reaper::ReaperConfig::from_env();
        if reaper_cfg.orphan_gc_enabled {
            let report =
                reaper::run_sweep(&core, wasm_host_handle.as_ref(), &reaper_cfg, true).await;
            if !report.is_empty() {
                tracing::warn!(
                    metric = "loom_reaper_startup_sweep",
                    orphan_browsers_killed = report.orphan_browsers_killed.len(),
                    orphan_dirs_removed = report.orphan_dirs_removed,
                    "startup orphan-Chromium GC reaped leaked browser trees"
                );
            }
        }
    }

    // 4. Build schema provider. The schema directory holds per-method
    //    JSON Schema files emitted at build time .
    //    If the directory is empty / missing, the daemon starts with an
    //    empty registry — `rpc.schemas` returns an empty method list
    //    and schema validation is bypassed (no method schemas = pass).
    //
    //..03: try the data_root location first
    //    (~/Library/Application Support/loom on macOS) then fall back
    //    to the postinstall-installed location (~/.config/loom). The
    //    `loom postinstall` runner historically writes to ~/.config/loom
    //    on macOS for cross-platform parity with the Linux build, while
    //    the daemon's data_root follows platform conventions. Allow
    //    either to satisfy the snapshot so `rpc.schemas` populates.
    let primary_schema_dir = args.data_root.join("schemas").join("v1");
    // The `loom postinstall` runner installs to ~/.config/loom on every
    // platform (cross-platform parity with the Linux build). On macOS,
    // `dirs::config_dir()` returns `~/Library/Application Support`, NOT
    // `~/.config` — so we hardcode the `$HOME/.config/loom` fallback to
    // match what the postinstall step actually writes.
    let postinstall_schema_dir = std::env::var_os("HOME")
        .map(|h| {
            std::path::PathBuf::from(h)
                .join(".config")
                .join("loom")
                .join("schemas")
                .join("v1")
        })
        .unwrap_or_else(|| std::path::PathBuf::from(".loom-schemas"));
    let schemas: Arc<dyn loom_rpc::schema_provider::schema_provider::SchemaProviderApi> =
        match SchemaProvider::load_at_startup(&primary_schema_dir) {
            Ok(s) => s,
            Err(_) => match SchemaProvider::load_at_startup(&postinstall_schema_dir) {
                Ok(s) => s,
                Err(_) => Arc::new(EmptySchemas),
            },
        };

    // 5. Wire DI graph.
    let core_adapter = CoreServiceAdapter::new(Arc::new(CoreBridge {
        core: Arc::clone(&core),
        wasm_host: wasm_host_handle.clone(),
        cleanup_tasks: Arc::new(std::sync::Mutex::new(tokio::task::JoinSet::new())),
    }));
    let host_adapter = HostServiceAdapter::new(host_bridge);
    let validator: Arc<dyn loom_rpc::schema_validator::schema_validator::SchemaValidatorApi> =
        SchemaValidator::new(Arc::clone(&schemas));
    let obs: Arc<dyn loom_rpc::rpc_observability::rpc_observability::RpcObservabilityApi> =
        RpcObservability::new();
    let handlers = RpcHandlers::new(
        core_adapter,
        host_adapter,
        Arc::clone(&schemas),
        Arc::clone(&validator),
        Arc::clone(&obs),
    );
    // Wire the async shim-teardown driver so `session.kill` can await
    // shim child reap with a 5 s ceiling per D12. When wasm_host is None
    // (chromium not yet postinstalled), session.kill degrades to
    // abort-only — caller still gets a typed envelope back.
    if let Some(host) = wasm_host_handle.clone() {
        let _ = handlers.set_session_shutdown(Arc::new(WasmHostShutdownAdapter { host }));
    }
    // Wire the daemon.health snapshot provider. Always wireable —
    // wasm_host being None just means `shim_breaker_states` returns
    // empty. Active-session count comes from the core facade regardless.
    // One bridge instance, two trait wirings (sync shallow + async deep).
    let bridge = Arc::new(DaemonHealthBridge {
        core: Arc::clone(&core),
        wasm_host: wasm_host_handle.clone(),
    });
    let _ = handlers.set_health_provider(bridge.clone() as Arc<dyn DaemonHealthProvider>);
    let _ = handlers.set_daemon_health_async(bridge as Arc<dyn DaemonHealthAsync>);
    let router: Arc<dyn loom_rpc::request_router::request_router::RequestRouterApi> =
        RequestRouter::register_methods(
            Arc::clone(&handlers),
            Arc::clone(&schemas),
            Arc::clone(&validator),
        )
        .map_err(|e| anyhow::anyhow!("RequestRouter::register_methods failed: {:?}", e))?;

    // 6. Bind socket. Generate token once; share between auth + socket config.
    let token = Token::generate();
    let token_arc = Arc::new(token.clone());
    let auth: Arc<dyn loom_rpc::auth_middleware::auth_middleware::AuthMiddlewareApi> =
        AuthMiddleware::new(Arc::clone(&token_arc));
    let socket_config = SocketServerConfig {
        socket_path: args.socket_path.clone(),
        token_override: Some(token),
    };
    let deps = Arc::new(ConnectionHandlerDeps {
        auth,
        validator: Arc::clone(&validator),
        router,
        observability: Arc::clone(&obs),
    });
    let server = SocketServer::new(socket_config, deps)
        .map_err(|e| anyhow::anyhow!("SocketServer::new failed: {:?}", e))?;

    // 7. Write auth artefacts for CLI (per the AuthManager contract):
    //    hello.token + daemon.pid in data_root/auth/.
    let auth_dir = args.data_root.join("auth");
    std::fs::create_dir_all(&auth_dir)
        .with_context(|| format!("create auth dir {}", auth_dir.display()))?;
    let token_path = auth_dir.join("hello.token");
    let pid_path = auth_dir.join("daemon.pid");

    // 7a. A-W8.1 / W8.5 0600 startup probe: refuse to start if a pre-
    //     existing auth file has loose mode bits (group/world readable
    //     or writable). Catches the "operator rsync'd $HOME with default
    //     umask and lost the 0600" class of incidents BEFORE the token
    //     is reused. Crash-only; no auto-chmod (the operator must
    //     consciously remediate so the audit trail records intent).
    probe_auth_perms_or_refuse(&token_path, "hello.token")?;
    probe_auth_perms_or_refuse(&pid_path, "daemon.pid")?;

    std::fs::write(&token_path, server.token.0.as_bytes())
        .with_context(|| format!("write hello.token to {}", token_path.display()))?;
    std::fs::write(&pid_path, std::process::id().to_string().as_bytes())
        .with_context(|| format!("write daemon.pid to {}", pid_path.display()))?;

    // 7b. A-W8.1 second leg: tighten the freshly-written files to 0600.
    //     The umask on default Linux installs is 0022 → files land at
    //     0644 → group + world can read the daemon's auth token. Set
    //     explicit perms so the file mode matches the socket's 0600
    //     contract (SOCKET_MODE in loom-rpc).
    apply_auth_perms_0600(&token_path, "hello.token")?;
    apply_auth_perms_0600(&pid_path, "daemon.pid")?;

    // 8. Print HELLO_TOKEN to stdout .
    println!("HELLO_TOKEN={}", server.token.0);

    // 9. Signal handler for graceful shutdown. SIGTERM is what launchd
    //    stop, systemd stop, and a plain `kill` against daemon.pid all
    //    deliver; SIGINT covers interactive Ctrl-C. Both resolve the same
    //    future so every routine service stop takes the graceful path
    //    below (drain accept loop, abort reaper, remove auth artefacts)
    //    instead of a hard kill that tears in-flight WAL appends.
    let shutdown = shutdown_signal();

    // 9b. Periodic reaper sweep: idle-session eviction + zombie detection + orphan-Chromium
    //     GC on a fixed cadence so a long-running daemon under churn stays healthy without
    //     manual intervention. Runs as a background task aborted on shutdown (below). Skipped
    //     entirely when neither idle-TTL nor orphan-GC is enabled.
    let reaper_task = {
        let reaper_cfg = reaper::ReaperConfig::from_env();
        if reaper_cfg.periodic_enabled() {
            let core_for_reaper = Arc::clone(&core);
            let host_for_reaper = wasm_host_handle.clone();
            tracing::info!(
                idle_ttl_secs = reaper_cfg.idle_ttl.as_secs(),
                sweep_secs = reaper_cfg.sweep_interval.as_secs(),
                orphan_gc = reaper_cfg.orphan_gc_enabled,
                "reaper: periodic sweep enabled"
            );
            Some(tokio::spawn(async move {
                let mut ticker = tokio::time::interval(reaper_cfg.sweep_interval);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                // First tick fires immediately; skip it so we don't double-run the
                // startup sweep that already executed above.
                ticker.tick().await;
                loop {
                    ticker.tick().await;
                    let report = reaper::run_sweep(
                        &core_for_reaper,
                        host_for_reaper.as_ref(),
                        &reaper_cfg,
                        true,
                    )
                    .await;
                    if !report.is_empty() {
                        tracing::info!(
                            metric = "loom_reaper_periodic_sweep",
                            idle_evicted = report.idle_evicted.len(),
                            zombies_closed = report.zombies_closed.len(),
                            orphan_browsers_killed = report.orphan_browsers_killed.len(),
                            orphan_dirs_removed = report.orphan_dirs_removed,
                            "reaper: periodic sweep reaped leaked resources"
                        );
                    }
                }
            }))
        } else {
            None
        }
    };

    // 10. Serve.
    let handle = tokio::runtime::Handle::current();
    server.serve(handle, shutdown).await;

    // 10b. Stop the reaper task on shutdown so it doesn't outlive the runtime.
    if let Some(task) = reaper_task {
        task.abort();
    }

    // 11. Cleanup auth artefacts on shutdown.
    let _ = std::fs::remove_file(&token_path);
    let _ = std::fs::remove_file(&pid_path);

    Ok(())
}

/// Future that resolves when a shutdown signal arrives: SIGINT (Ctrl-C)
/// or — on unix — SIGTERM (the launchd/systemd/`kill` default). Fulfils
/// the module-doc contract "Block on the accept loop until SIGINT /
/// SIGTERM". Handler-installation failure logs and falls back to the
/// other signal instead of panicking: an `.expect()` here would panic
/// the shutdown future inside `SocketServer::serve`'s `select!` and
/// tear down the accept loop.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "failed to install Ctrl-C handler; relying on SIGTERM");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    {
        let sigterm = async {
            use tokio::signal::unix::{signal, SignalKind};
            match signal(SignalKind::terminate()) {
                Ok(mut stream) => {
                    stream.recv().await;
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to install SIGTERM handler; relying on Ctrl-C");
                    std::future::pending::<()>().await;
                }
            }
        };
        tokio::select! {
            () = ctrl_c => {}
            () = sigterm => {}
        }
    }
    #[cfg(not(unix))]
    ctrl_c.await;
}

/// Build a WasmHostBridge. Tries to create a real `WasmHost`; if the
/// surfaces directory is missing or modules haven't been compiled yet,
/// falls back to the stub that returns `SurfaceUnavailable`.
///
/// Returns both the bridge (for the host service adapter) and the
/// underlying `Arc<WasmHost>` (for the CoreBridge so session-close can
/// trigger shim teardown). When a real WasmHost couldn't be built, the
/// second slot is None.
fn build_host_bridge(
    core: Arc<CoreApiFacade>,
    upload_root: Option<PathBuf>,
) -> (Arc<dyn WasmHostBridge>, Option<Arc<loom_host::WasmHost>>) {
    use loom_host::{HostConfig, ShimChromiumConfig, WasmHost};

    // Surface the upload-root posture at startup so operators aren't left
    // guessing why web.set_input_files fails closed (it denies all uploads
    // when LOOM_UPLOAD_ROOT is unset).
    match &upload_root {
        Some(root) => tracing::info!(
            upload_root = %root.display(),
            "web.set_input_files uploads enabled (LOOM_UPLOAD_ROOT)"
        ),
        None => tracing::info!(
            "web.set_input_files uploads DISABLED — set LOOM_UPLOAD_ROOT to enable (fail-closed)"
        ),
    }

    // Resolve surfaces dir the same way `loom postinstall` writes them
    // (~/.config/loom/surfaces/) so AOT-compiled .cwasm modules are found.
    // The CLI's compiled_defaults() hardcodes `home.join(".config").join("loom")`
    // (cli_config/interfaces.rs), so the daemon MUST mirror that path verbatim.
    // dirs::config_dir() returns ~/Library/Application Support on macOS — wrong.
    let surfaces_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".config")
        .join("loom")
        .join("surfaces");

    // resolve Chromium across all install channels. Resolution
    // chain: LOOM_CHROMIUM_PATH env override → pinned `~/.config/loom/chromium/...`
    // → PATH search (chromium / chromium-browser / chrome / google-chrome) →
    // macOS `/Applications/...` → typed `BrowserNotFound`. Pinned wins for
    // replay-bit-equality ; a `tracing::warn!` fires below when
    // the resolver picks a non-pinned source so users know they've lost
    // determinism.
    let chromium_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".config")
        .join("loom")
        .join("chromium");
    let shim_chromium = match loom_shared::chromium_resolver::resolve_chromium(&chromium_dir) {
        Ok((chromium, source)) => {
            use loom_shared::chromium_resolver::ChromiumSource;
            if source != ChromiumSource::Pinned {
                tracing::warn!(
                    chromium_path = %chromium.display(),
                    source = ?source,
                    "loom: using system-installed Chromium; replay-bit-equality is \
                     not guaranteed across machines. Run 'loom postinstall' for the \
                     pinned build."
                );
            }
            loom_shared::binary_resolver::resolve_loom_sibling("loom-shim-chromium").map(
                |shim_bin| ShimChromiumConfig {
                    shim_binary_path: shim_bin,
                    chromium_path: chromium,
                },
            )
        }
        Err(not_found) => {
            tracing::error!(
                searched = ?not_found.searched_paths,
                "Chromium not found by any resolver path. \
                 Install via 'brew install --cask chromium' (macOS) or your \
                 distro's package manager (Linux), or run 'loom postinstall' \
                 for the pinned build. session.create will return BrowserNotFound \
                 until Chromium is reachable."
            );
            None
        }
    };

    let has_chromium = shim_chromium.is_some();
    let host_config = HostConfig {
        surfaces_dir,
        shim_chromium,
        ..HostConfig::default()
    };

    match WasmHost::new(Arc::clone(&core), host_config) {
        Ok(host) => {
            let host_for_bridge = Arc::clone(&host);
            (
                Arc::new(WasmBridge {
                    host: host_for_bridge,
                    core,
                    has_chromium,
                    upload_root,
                }),
                Some(host),
            )
        }
        Err(_) => {
            tracing::warn!(
                "WasmHost unavailable — run `loom postinstall` to compile surface modules"
            );
            (Arc::new(StubHostBridge), None)
        }
    }
}

// ─── Minimal empty schema provider ──────────────────────────────────────────

/// Used when the schema directory doesn't exist yet (pre-postinstall).
struct EmptySchemas;

impl loom_rpc::schema_provider::schema_provider::SchemaProviderApi for EmptySchemas {
    fn lookup_request_schema(
        &self,
        _method: &str,
    ) -> Option<Arc<loom_rpc::schema_provider::schema_provider::CompiledJsonSchema>> {
        None
    }
    fn lookup_response_schema(
        &self,
        _method: &str,
    ) -> Option<Arc<loom_rpc::schema_provider::schema_provider::CompiledJsonSchema>> {
        None
    }
    fn registered_methods(&self) -> Vec<String> {
        vec![]
    }
    fn get_registry_snapshot(&self) -> loom_rpc::schema_provider::schema_provider::SchemaRegistry {
        loom_rpc::schema_provider::schema_provider::SchemaRegistry {
            methods: vec![],
            source_wit_sha256: String::new(),
        }
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod upload_guard_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use loom_shared::shim_protocol::CdpMessage;

    // ─── Vault label validation (D37) ──────────
    // Direct coverage for the canonical rule shared by vault_set_secret /
    // vault_delete_secret via validate_label_or_rpc_err.

    #[test]
    fn validate_label_canonical_accepts_valid_labels() {
        for ok in ["gh", "github:token", "my-label_1", &"a".repeat(64)] {
            assert!(
                validate_label_canonical(ok).is_ok(),
                "expected {ok:?} to be accepted"
            );
        }
    }

    #[test]
    fn validate_label_canonical_rejects_invalid_labels() {
        assert!(validate_label_canonical("").is_err(), "empty");
        assert!(
            validate_label_canonical(&"a".repeat(65)).is_err(),
            "over 64 chars"
        );
        for bad in [
            "has space",
            "slash/here",
            "dot.here",
            "emoji😀",
            "tab\there",
        ] {
            assert!(
                validate_label_canonical(bad).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn validate_label_or_rpc_err_maps_rejection_to_invalid_argument() {
        // Valid label passes through.
        assert!(validate_label_or_rpc_err("gh").is_ok());
        // Invalid label maps to the same adapter error as InvalidArgument.
        let err = validate_label_or_rpc_err("bad/label").expect_err("should reject");
        let expected = map_loom_error(&LoomError::new(
            loom_core::error::LoomErrorCode::InvalidArgument,
            "x",
        ));
        assert_eq!(
            err, expected,
            "rejection must map to the same adapter error code as InvalidArgument"
        );
    }

    // ─── Daemon-layer evaluate gate (Layer B) ──────────

    /// receipt envelope shape after daemon-side rejection.
    /// Pins the wire fields the operator's `loom action web.evaluate`
    /// reproducer reads from stdout JSON.
    #[test]
    fn profile_restricted_evaluate_receipt_carries_required_fields() {
        let receipt = profile_restricted_evaluate_receipt(42, "01HZSESSION", "window.location");
        assert_eq!(receipt.action_id, 42);
        assert_eq!(receipt.session_id, "01HZSESSION");
        assert!(matches!(
            receipt.status,
            loom_rpc::host_service_adapter::host_service_adapter::ReceiptStatus::Error
        ));
        let err = receipt.error.expect("error envelope present");
        assert_eq!(err.kind, "profile_restricted");
        let detail = err.detail.expect("detail present");
        assert_eq!(detail["matched_pattern"], "window.location");
        assert_eq!(detail["profile"], "safe");
        assert_eq!(detail["violation"], "safe_profile_evaluate_denylist_match");
        // Tier-2 navigate fields should all be None on a synthesized
        // error receipt — no DOM/screenshot/network on a refused action.
        assert!(receipt.url.is_none());
        assert!(receipt.dom_snapshot_hash.is_none());
        assert!(receipt.network_summary.is_none());
        assert_eq!(receipt.timing_ticks, 0);
    }

    /// verify the operator's exact reproducer pattern
    /// matches the daemon's denylist BEFORE shim dispatch.
    #[test]
    fn evaluate_denylist_blocks_operator_reproducer_window_location_assignment() {
        let expr = "window.location.href = \"https://evil.example.com\"";
        let matched = loom_shared::safety::EVALUATE_DENYLIST
            .iter()
            .find(|p| expr.contains(*p));
        assert_eq!(matched, Some(&"window.location"));
    }

    /// service-worker registration is gated; feature detection is allowed.
    #[test]
    fn evaluate_denylist_gates_service_worker_register_not_feature_detect() {
        let register = "navigator.serviceWorker.register('/sw.js')";
        let detect = "if ('serviceWorker' in navigator) {}";
        assert!(
            loom_shared::safety::EVALUATE_DENYLIST
                .iter()
                .any(|p| register.contains(*p)),
            "registration must be blocked"
        );
        assert!(
            !loom_shared::safety::EVALUATE_DENYLIST
                .iter()
                .any(|p| detect.contains(*p)),
            "feature detection must NOT be blocked"
        );
    }

    /// Wire string — `LoomErrorCode::ProfileRestricted`
    /// serializes as `"profile_restricted"`. Mirrors what the receipt's
    /// `error.kind` carries; if these drift, the operator's grep on
    /// the receipt JSON breaks.
    #[test]
    fn loom_error_code_profile_restricted_wire_string_matches_receipt_kind() {
        use loom_shared::error_format::LoomErrorCode;
        assert_eq!(
            LoomErrorCode::ProfileRestricted.as_wire(),
            "profile_restricted"
        );
    }

    // ─── v0.9.7 follow-ups A + B — cookie validation + grant ────────────

    /// Each `CookieValidationError` variant maps to a stable snake_case
    /// wire string. The operator's `loom action web.set_cookies` failure
    /// receipt carries `detail.code = <wire string>` so dashboards can
    /// group by validation reason.
    #[test]
    fn cookie_validation_code_covers_all_variants() {
        use loom_shared::cookie_types::CookieValidationError as E;
        assert_eq!(cookie_validation_code(&E::NameEmpty), "name_empty");
        assert_eq!(
            cookie_validation_code(&E::NameInvalid { ch: ';' }),
            "name_invalid"
        );
        assert_eq!(
            cookie_validation_code(&E::ValueTooLarge { size: 5_000 }),
            "value_too_large"
        );
        assert_eq!(
            cookie_validation_code(&E::InvalidSameSite("foo".to_string())),
            "invalid_same_site"
        );
        assert_eq!(
            cookie_validation_code(&E::InvalidExpires(f64::NAN)),
            "invalid_expires"
        );
        assert_eq!(
            cookie_validation_code(&E::TooManyCookies(65)),
            "too_many_cookies"
        );
    }

    /// Receipt envelope shape returned when daemon-side validation
    /// rejects a `web.set_cookies` payload. Pins the wire fields the
    /// CLI reproducer reads (`error.kind` and `error.detail.code`).
    #[test]
    fn cookie_validation_error_receipt_carries_required_fields() {
        let r = cookie_validation_error_receipt(
            7,
            "01HZSESSION",
            "too_many_cookies",
            "65 cookies provided, max is 64".to_string(),
        );
        assert_eq!(r.action_id, 7);
        assert_eq!(r.session_id, "01HZSESSION");
        assert!(matches!(
            r.status,
            loom_rpc::host_service_adapter::host_service_adapter::ReceiptStatus::Error
        ));
        let err = r.error.expect("error envelope present");
        assert_eq!(err.kind, "cookie_validation_error");
        let detail = err.detail.expect("detail present");
        assert_eq!(detail["code"], "too_many_cookies");
        assert_eq!(detail["message"], "65 cookies provided, max is 64");
        // Synthesised error — none of the success-path fields populated.
        assert!(r.set_cookies_result.is_none());
        assert!(r.url.is_none());
        assert!(r.dom_snapshot_hash.is_none());
        assert_eq!(r.timing_ticks, 0);
    }

    /// `build_chromium_args` defensively emits an empty no-op envelope
    /// for a `set_cookies` action whose source is still `grant` by the
    /// time it reaches the CDP encoder — the dispatcher should have
    /// resolved it upstream, but tests / future callers may bypass
    /// that path.
    #[test]
    fn build_chromium_args_set_cookies_grant_source_emits_empty_no_op() {
        let action = Action::WebSetCookies {
            session_id: s("sess"),
            source: serde_json::json!({
                "source": "grant",
                "grant_id": "grn_abc",
            }),
        };
        let msg = decode_cdp(&action).expect("envelope produced");
        assert_eq!(msg.method, "Network.setCookies");
        // params.cookies = [] (empty array)
        let cookies = params_get(&msg, "cookies").expect("cookies param present");
        match cookies {
            ciborium::value::Value::Array(arr) => assert!(arr.is_empty()),
            other => panic!("expected empty array, got {other:?}"),
        }
    }

    /// `build_chromium_args` for a resolved inline `set_cookies` passes
    /// the cookies array through to the CDP envelope. This is the
    /// shape the dispatcher hands `build_chromium_args` after grant
    /// resolution.
    #[test]
    fn build_chromium_args_set_cookies_inline_passes_through_cookies() {
        let action = Action::WebSetCookies {
            session_id: s("sess"),
            source: serde_json::json!({
                "source": "inline",
                "cookies": [
                    {"name": "sid", "value": "abc", "domain": "example.com", "path": "/"}
                ],
            }),
        };
        let msg = decode_cdp(&action).expect("envelope produced");
        assert_eq!(msg.method, "Network.setCookies");
        let cookies = params_get(&msg, "cookies").expect("cookies param present");
        match cookies {
            ciborium::value::Value::Array(arr) => {
                assert_eq!(arr.len(), 1);
            }
            other => panic!("expected array, got {other:?}"),
        }
    }

    // ─── existing tests ─────────────────────────────────────────────────

    /// Decode `build_chromium_args` output into the wire-shape struct.
    /// Returns None if the function returned None (legacy fallback path).
    fn decode_cdp(action: &Action) -> Option<CdpMessage> {
        let bytes = build_chromium_args(action)?;
        Some(
            ciborium::de::from_reader::<CdpMessage, _>(bytes.as_slice()).expect("valid CdpMessage"),
        )
    }

    fn s(v: &str) -> String {
        v.to_string()
    }

    fn params_get<'a>(msg: &'a CdpMessage, key: &str) -> Option<&'a ciborium::value::Value> {
        match &msg.params {
            ciborium::value::Value::Map(entries) => entries.iter().find_map(|(k, v)| match k {
                ciborium::value::Value::Text(t) if t == key => Some(v),
                _ => None,
            }),
            _ => None,
        }
    }

    fn expr_of(msg: &CdpMessage) -> &str {
        match params_get(msg, "expression").expect("expression param") {
            ciborium::value::Value::Text(t) => t.as_str(),
            _ => panic!("expression not a Text"),
        }
    }

    /// every Web.* variant produces a decodable CdpMessage.
    #[test]
    fn build_chromium_args_emits_valid_cdp_message_for_each_web_verb() {
        let session = s("sess-1");
        let cases: Vec<(Action, &str)> = vec![
            (
                Action::WebNavigate {
                    session_id: session.clone(),
                    url: s("https://example.com"),
                    until: None,
                    timeout_ms: None,
                },
                "Page.navigate",
            ),
            (
                Action::WebClick {
                    session_id: session.clone(),
                    selector: s("a"),
                },
                "Runtime.evaluate",
            ),
            (
                Action::WebEvaluate {
                    session_id: session.clone(),
                    expression: s("1+1"),
                },
                "Runtime.evaluate",
            ),
            (
                Action::WebType {
                    session_id: session.clone(),
                    selector: s("input"),
                    text: s("hello"),
                },
                "Runtime.evaluate",
            ),
            (
                Action::WebSelect {
                    session_id: session.clone(),
                    selector: s("select"),
                    value: s("v1"),
                },
                "Runtime.evaluate",
            ),
            (
                Action::WebHover {
                    session_id: session.clone(),
                    selector: s("a"),
                },
                "Runtime.evaluate",
            ),
            (
                Action::WebScroll {
                    session_id: session.clone(),
                    selector: s("body"),
                    delta_x: Some(0),
                    delta_y: Some(100),
                },
                "Runtime.evaluate",
            ),
            (
                Action::WebWait {
                    session_id: session.clone(),
                    selector: s("a"),
                    timeout_ms: Some(1000),
                },
                "Runtime.evaluate",
            ),
            (
                Action::WebScreenshot {
                    session_id: session.clone(),
                    selector: None,
                },
                "Page.captureScreenshot",
            ),
            (
                Action::WebSnapshot {
                    session_id: session.clone(),
                },
                "DOM.getDocument",
            ),
        ];
        for (action, expected_method) in cases {
            let msg = decode_cdp(&action)
                .unwrap_or_else(|| panic!("build_chromium_args returned None for {action:?}"));
            assert_eq!(msg.method, expected_method, "wrong method for {action:?}");
        }
    }

    /// web.snapshot must capture shadow DOM + iframe content (`pierce:true`),
    /// matching web.navigate (shim STEP 5, already pierce:true) so the two DOM
    /// captures hash a comparable node set. Locks the unified contract against
    /// silent regression — see specs/2026-06-09-unify-pierce-setting/plan.md.
    #[test]
    fn build_chromium_args_snapshot_emits_pierce_true() {
        let action = Action::WebSnapshot {
            session_id: s("sess-1"),
        };
        let msg = decode_cdp(&action).expect("Some");
        assert_eq!(msg.method, "DOM.getDocument");
        match params_get(&msg, "pierce").expect("pierce param") {
            ciborium::value::Value::Bool(b) => {
                assert!(*b, "snapshot DOM.getDocument must use pierce:true")
            }
            other => panic!("pierce not a Bool: {other:?}"),
        }
    }

    /// evaluate carries the user expression verbatim.
    #[test]
    fn build_chromium_args_evaluate_emits_runtime_evaluate_with_expression() {
        let action = Action::WebEvaluate {
            session_id: s("sess"),
            expression: s("1+1"),
        };
        let msg = decode_cdp(&action).expect("Some");
        assert_eq!(msg.method, "Runtime.evaluate");
        assert_eq!(expr_of(&msg), "1+1");
    }

    /// click selects + clicks via Runtime.evaluate.
    #[test]
    fn build_chromium_args_click_emits_runtime_evaluate_with_query_selector_click() {
        let action = Action::WebClick {
            session_id: s("sess"),
            selector: s("a"),
        };
        let msg = decode_cdp(&action).expect("Some");
        assert_eq!(msg.method, "Runtime.evaluate");
        let expr = expr_of(&msg);
        assert!(
            expr.contains("document.querySelector(\"a\")"),
            "got: {expr}"
        );
        assert!(expr.contains(".click()"), "got: {expr}");
    }

    /// type sets value via the framework-aware native setter (so React/Vue/Angular
    /// trackers see the change) AND dispatches input/change events.
    #[test]
    fn build_chromium_args_type_emits_runtime_evaluate_setting_value_and_dispatching_input_change()
    {
        let action = Action::WebType {
            session_id: s("sess"),
            selector: s("input"),
            text: s("hello"),
        };
        let msg = decode_cdp(&action).expect("Some");
        assert_eq!(msg.method, "Runtime.evaluate");
        let expr = expr_of(&msg);
        // Framework-aware: must call the prototype's value setter, not assign
        // `.value =` directly (which bypasses React's tracker).
        assert!(
            expr.contains("setter.call(el,"),
            "expected setter.call(el, ...) in {expr}"
        );
        assert!(
            expr.contains("HTMLInputElement.prototype"),
            "expected HTMLInputElement.prototype in {expr}"
        );
        assert!(
            !expr.contains(";el.value="),
            "regression: direct el.value= bypasses React tracker, in {expr}"
        );
        assert!(
            expr.contains("new Event('input'"),
            "expected input event in {expr}"
        );
        assert!(
            expr.contains("new Event('change'"),
            "expected change event in {expr}"
        );
    }

    /// screenshot uses Page.captureScreenshot { format: "png" }.
    #[test]
    fn build_chromium_args_screenshot_emits_page_capture_screenshot_png() {
        let action = Action::WebScreenshot {
            session_id: s("sess"),
            selector: None,
        };
        let msg = decode_cdp(&action).expect("Some");
        assert_eq!(msg.method, "Page.captureScreenshot");
        match params_get(&msg, "format").expect("format param") {
            ciborium::value::Value::Text(t) => assert_eq!(t, "png"),
            other => panic!("format not text: {other:?}"),
        }
    }

    // === build_wire_receipt_error: wire shape ===

    #[test]
    fn build_wire_receipt_error_shim_failure_with_typed_http_status_detail() {
        let detail =
            r#"{"kind":"http_status","url":"http://fake.test/status/404","status_code":404}"#;
        let err = build_wire_receipt_error("shim-failure", Some(detail));
        assert_eq!(err.kind, "http_status");
        let d = err.detail.as_ref().expect("detail must be present");
        assert_eq!(
            d.get("url").and_then(|v| v.as_str()),
            Some("http://fake.test/status/404")
        );
        assert_eq!(d.get("status_code").and_then(|v| v.as_u64()), Some(404));
        // `kind` must NOT be in detail — it's been hoisted to the wire kind field.
        assert!(
            d.get("kind").is_none(),
            "kind should be hoisted, not in detail"
        );
    }

    #[test]
    fn build_wire_receipt_error_shim_failure_with_typed_dns_failure_detail() {
        let detail = r#"{"kind":"dns_failure","url":"http://fake.test/error/x","chromium_error":"net::ERR_NAME_NOT_RESOLVED"}"#;
        let err = build_wire_receipt_error("shim-failure", Some(detail));
        assert_eq!(err.kind, "dns_failure");
        let d = err.detail.as_ref().expect("detail must be present");
        assert_eq!(
            d.get("chromium_error").and_then(|v| v.as_str()),
            Some("net::ERR_NAME_NOT_RESOLVED")
        );
    }

    #[test]
    fn build_wire_receipt_error_untyped_shim_failure_falls_back_to_message() {
        // Plain-string shim-failure (not structured JSON) — the raw string
        // becomes detail.message; kind keeps the raw error_code.
        let err = build_wire_receipt_error("shim-failure", Some("chromium subprocess died"));
        assert_eq!(err.kind, "shim-failure");
        let d = err.detail.as_ref().expect("detail must be present");
        assert_eq!(
            d.get("message").and_then(|v| v.as_str()),
            Some("chromium subprocess died")
        );
    }

    #[test]
    fn build_wire_receipt_error_non_shim_failure_uses_code_as_kind() {
        let err = build_wire_receipt_error("budget-exceeded", Some("navigate exceeded 30s"));
        assert_eq!(err.kind, "budget-exceeded");
        let d = err.detail.as_ref().expect("detail must be present");
        assert_eq!(
            d.get("message").and_then(|v| v.as_str()),
            Some("navigate exceeded 30s")
        );
    }

    #[test]
    fn build_wire_receipt_error_empty_details_yields_no_detail() {
        let err = build_wire_receipt_error("internal", None);
        assert_eq!(err.kind, "internal");
        assert!(err.detail.is_none());
        let err2 = build_wire_receipt_error("internal", Some(""));
        assert!(err2.detail.is_none());
    }

    /// Security: selector strings containing JS metacharacters must be JSON-escaped.
    #[test]
    fn build_chromium_args_click_quotes_selector_with_double_quote_in_it() {
        // selector contains a literal double-quote character: a[id="x']
        let selector = "a[id=\"x']".to_string();
        let action = Action::WebClick {
            session_id: s("sess"),
            selector: selector.clone(),
        };
        let msg = decode_cdp(&action).expect("Some");
        let expr = expr_of(&msg);
        // Must contain the JSON-escaped form: "a[id=\"x']"
        assert!(
            expr.contains("\"a[id=\\\"x']\""),
            "selector not JSON-escaped; expr was: {expr}"
        );
        // Must NOT contain a raw unescaped double-quote inside the literal that
        // would have closed the JS string early.
        // Validate by parsing the expression: it should still be a syntactically
        // closeable JS source — at minimum, count of unescaped double-quotes
        // should be even (open+close pairs).
        let mut escaped = false;
        let mut quotes = 0usize;
        for ch in expr.chars() {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => quotes += 1,
                _ => {}
            }
        }
        assert_eq!(
            quotes % 2,
            0,
            "odd number of unescaped quotes in expr: {expr}"
        );
    }

    // ─── Tests for build_navigate_wire_receipt ─────────────────────────
    //
    // These pin the production wire-receipt construction path that
    // `WasmHostBridge::dispatch_action_blocking` invokes. Required because
    // every other test for the daemon's dispatch path stubs the trait
    // (`loom-rpc/tests/...`) or stops at the shim layer
    // (`loom-host/tests/integration_navigate_tier2_e2e.rs`); without
    // these, the `Receipt` construction + JSON-decode + capture-policy
    // arms would ship with no direct test coverage.

    use loom_core::receipt_builder::receipt_builder::NetworkSummary;
    use loom_host::receipt_marshaller::{ReceiptBuilder, ReceiptStatus as HostStatus};
    use loom_shared::navigate_outcome::{LoomNetworkEvent, ShimConsoleLine};

    fn nav_event(status: u16, bytes: u64) -> LoomNetworkEvent {
        LoomNetworkEvent {
            method: "GET".into(),
            url: "https://example.com/x".into(),
            request_hash: "0".repeat(64),
            response_hash: "1".repeat(64),
            status,
            content_type: "text/html".into(),
            duration_ms: 50,
            response_bytes: bytes,
            error_reason: None,
            error_kind: None,
        }
    }

    fn navigate_builder_with_all_blobs() -> ReceiptBuilder {
        let console_lines = vec![ShimConsoleLine {
            level: "info".into(),
            message: "ready".into(),
        }];
        let summary = NetworkSummary {
            total_count: 2,
            total_bytes: 5120,
            error_count: 0,
        };
        let events = vec![nav_event(200, 4096), nav_event(200, 1024)];
        ReceiptBuilder {
            action_id: 11,
            finished_at_ms: 250,
            started_at_ms: 0,
            status: HostStatus::Ok,
            action_hash: "aa".repeat(32),
            outcome_hash: "bb".repeat(32),
            emitted_at_ms: 1_714_074_336_000,
            navigate_url: Some("https://example.com/".into()),
            navigate_final_url: Some("https://example.com/".into()),
            navigate_title: Some("Example".into()),
            navigate_status_code: Some(200),
            navigate_dom_snapshot_hash: Some("a".repeat(64)),
            navigate_screenshot_after_hash: Some("b".repeat(64)),
            navigate_console_count: Some(1),
            navigate_network_count: Some(2),
            navigate_side_effects_json: Some(serde_json::to_vec(&events).unwrap()),
            navigate_console_lines_json: Some(serde_json::to_vec(&console_lines).unwrap()),
            navigate_network_summary_json: Some(serde_json::to_vec(&summary).unwrap()),
            ..Default::default()
        }
    }

    ///..04: default profile carries every brief-listed key
    /// when the upstream JSON blobs are well-formed. Tests the actual
    /// production decode path.
    #[test]
    fn build_navigate_wire_receipt_decodes_all_three_json_blobs_under_default() {
        let builder = navigate_builder_with_all_blobs();
        let r = build_navigate_wire_receipt(&builder, "S1", None);

        assert_eq!(r.action_id, 11);
        assert_eq!(r.session_id, "S1");
        assert_eq!(r.url.as_deref(), Some("https://example.com/"));
        assert_eq!(r.status_code, Some(200));
        assert_eq!(r.dom_snapshot_hash.as_ref().map(String::len), Some(64));
        assert_eq!(r.screenshot_after_hash.as_ref().map(String::len), Some(64));
        assert_eq!(r.console_lines.len(), 1);
        assert_eq!(r.console_lines[0].level, "info");
        let s = r.network_summary.as_ref().expect("network_summary present");
        assert_eq!(s.total_count, 2);
        assert_eq!(s.total_bytes, 5120);
        assert_eq!(s.error_count, 0);
        assert_eq!(r.side_effects.len(), 2);
        assert_eq!(r.side_effects[0]["method"], "GET");
        assert_eq!(r.side_effects[0]["status"], 200);
    }

    /// --capture-policy minimal strips tier-2 fields
    /// at the wire boundary. This is the test that actually exercises
    /// the `apply_capture_profile_to_wire(...)` invocation in the
    /// production code path.
    #[test]
    fn build_navigate_wire_receipt_minimal_strips_per_brief() {
        let builder = navigate_builder_with_all_blobs();
        let r = build_navigate_wire_receipt(&builder, "S1", Some("minimal"));

        // Identity + brief-listed survivors:
        assert_eq!(r.action_id, 11);
        assert_eq!(r.session_id, "S1");
        assert_eq!(r.url.as_deref(), Some("https://example.com/"));
        assert_eq!(r.status_code, Some(200));

        // Stripped:
        assert!(r.dom_snapshot_hash.is_none());
        assert!(r.screenshot_after_hash.is_none());
        assert!(r.console_lines.is_empty());
        assert!(r.network_summary.is_none());
        assert!(r.network_count.is_none());
        assert!(r.console_count.is_none());
        assert!(r.final_url.is_none());
        assert!(r.title.is_none());
        assert!(r.side_effects.is_empty());
        assert!(r.action_hash.is_none());
        assert!(r.outcome_hash.is_none());
        assert!(r.emitted_at_ms.is_none());
    }

    /// network_entries side-channel surfaces inline AND leaves the
    /// existing hashed/aggregate fields (network_count, side_effects,
    /// network_summary) byte-identical (backward-compat: separate path).
    #[test]
    fn build_navigate_wire_receipt_surfaces_inline_network_entries() {
        let mut builder = navigate_builder_with_all_blobs();
        let entries = vec![loom_shared::navigate_outcome::LoomNetworkEntry {
            url: "https://app.test/api/thing".into(),
            method: "GET".into(),
            status: 200,
            resource_type: "XHR".into(),
            from_cache: false,
            request_id: "R-1".into(),
            ts_ms: 1_700_000_000_000,
        }];
        builder.navigate_network_entries_json = Some(serde_json::to_vec(&entries).unwrap());
        let r = build_navigate_wire_receipt(&builder, "S1", None);

        // network_entries surfaced.
        assert_eq!(r.network_entries.len(), 1);
        assert_eq!(r.network_entries[0]["method"], "GET");
        assert_eq!(r.network_entries[0]["status"], 200);
        assert_eq!(r.network_entries[0]["resource_type"], "XHR");
        assert!(r.network_entries_blob_ref.is_none());

        // Backward-compat: the existing fields are untouched by the new path.
        assert_eq!(r.network_count, Some(2));
        assert_eq!(r.side_effects.len(), 2);
        assert!(r.network_summary.is_some());
    }

    /// When the host offloaded the list, the wire receipt carries the
    /// blob_ref (sha256) and an EMPTY inline list — the inline-XOR-blob
    /// discriminator, mirroring return_value_blob_ref.
    #[test]
    fn build_navigate_wire_receipt_surfaces_network_entries_blob_ref() {
        let mut builder = navigate_builder_with_all_blobs();
        builder.navigate_network_entries_json = None;
        builder.navigate_network_entries_blob_ref = Some(loom_core::content_store::ContentRef {
            sha256: "c".repeat(64),
            size_bytes: 70_000,
        });
        builder.navigate_network_entries_truncated = Some(false);
        let r = build_navigate_wire_receipt(&builder, "S1", None);

        assert!(r.network_entries.is_empty());
        assert_eq!(
            r.network_entries_blob_ref.as_ref().map(String::len),
            Some(64)
        );
    }

    /// --capture-policy minimal strips the observational network_entries.
    #[test]
    fn build_navigate_wire_receipt_minimal_strips_network_entries() {
        let mut builder = navigate_builder_with_all_blobs();
        let entries = vec![loom_shared::navigate_outcome::LoomNetworkEntry {
            url: "https://app.test/x".into(),
            method: "GET".into(),
            status: 200,
            resource_type: "Fetch".into(),
            from_cache: false,
            request_id: "R-1".into(),
            ts_ms: 1,
        }];
        builder.navigate_network_entries_json = Some(serde_json::to_vec(&entries).unwrap());
        builder.navigate_network_entries_truncated = Some(true);
        let r = build_navigate_wire_receipt(&builder, "S1", Some("minimal"));
        assert!(r.network_entries.is_empty());
        assert!(r.network_entries_blob_ref.is_none());
        assert!(r.network_entries_truncated.is_none());
    }

    /// `capture_policy_str = Some("default")` and `Some("full")` are
    /// no-ops on the wire today; Full will gain `dom_full_text`
    /// semantics in a future PR.
    #[test]
    fn build_navigate_wire_receipt_default_and_full_are_noops() {
        let builder = navigate_builder_with_all_blobs();
        let none_r = build_navigate_wire_receipt(&builder, "S", None);
        let default_r = build_navigate_wire_receipt(&builder, "S", Some("default"));
        let full_r = build_navigate_wire_receipt(&builder, "S", Some("full"));

        let to_json = |r: &Receipt| serde_json::to_value(r).unwrap();
        assert_eq!(to_json(&none_r), to_json(&default_r));
        assert_eq!(to_json(&none_r), to_json(&full_r));
    }

    /// Decode-failure paths: malformed JSON in any of the three navigate
    /// blobs degrades to empty/None instead of failing the navigate
    /// (observability fields shouldn't trap). This pins the
    /// `tracing::warn` arms.
    #[test]
    fn build_navigate_wire_receipt_degrades_on_malformed_console_lines_json() {
        let mut builder = navigate_builder_with_all_blobs();
        builder.navigate_console_lines_json = Some(b"not valid json".to_vec());
        let r = build_navigate_wire_receipt(&builder, "S", None);
        assert!(
            r.console_lines.is_empty(),
            "must degrade to empty, not panic"
        );
        // Other fields unaffected:
        assert_eq!(r.url.as_deref(), Some("https://example.com/"));
    }

    #[test]
    fn build_navigate_wire_receipt_degrades_on_malformed_network_summary_json() {
        let mut builder = navigate_builder_with_all_blobs();
        builder.navigate_network_summary_json = Some(b"{not json".to_vec());
        let r = build_navigate_wire_receipt(&builder, "S", None);
        assert!(
            r.network_summary.is_none(),
            "must degrade to None, not panic"
        );
    }

    #[test]
    fn build_navigate_wire_receipt_degrades_on_malformed_side_effects_json() {
        let mut builder = navigate_builder_with_all_blobs();
        builder.navigate_side_effects_json = Some(b"[not events".to_vec());
        let r = build_navigate_wire_receipt(&builder, "S", None);
        assert!(
            r.side_effects.is_empty(),
            "must degrade to empty, not panic"
        );
    }

    /// Unknown capture-policy string falls back to Default (no-op) —
    /// validation is upstream in `session_validation::validate`. This
    /// ensures a stale / unparseable persisted value doesn't crash
    /// dispatch on an existing session.
    #[test]
    fn build_navigate_wire_receipt_unknown_policy_string_falls_back_to_default() {
        let builder = navigate_builder_with_all_blobs();
        let unknown = build_navigate_wire_receipt(&builder, "S", Some("bogus-profile"));
        let default = build_navigate_wire_receipt(&builder, "S", Some("default"));
        assert_eq!(
            serde_json::to_value(&unknown).unwrap(),
            serde_json::to_value(&default).unwrap(),
            "unknown policy must fall back to Default, not strip / no-op differently"
        );
    }

    // ─── Graceful shutdown signals (SIGINT + SIGTERM) ──────────
    // `kill <daemon.pid>` (SIGTERM — what launchd stop, systemd stop and a
    // plain `kill` deliver) must resolve the shutdown future so
    // `SocketServer::serve` drains instead of the process being hard-killed
    // mid-WAL-append. The tests raise a real signal at this test process:
    // once tokio's handler is installed the default kill disposition is
    // replaced, so the signal is observed by the stream rather than
    // terminating the test binary (workspace tests run --test-threads=1).

    #[tokio::test]
    async fn shutdown_signal_resolves_on_sigterm() {
        let task = tokio::spawn(shutdown_signal());
        // Let the spawned future poll once so the handlers are registered
        // before the signal is raised.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        unsafe {
            libc::kill(std::process::id() as libc::pid_t, libc::SIGTERM);
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .expect("shutdown future must resolve on SIGTERM")
            .expect("shutdown future must not panic");
    }

    #[tokio::test]
    async fn shutdown_signal_resolves_on_sigint() {
        let task = tokio::spawn(shutdown_signal());
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        unsafe {
            libc::kill(std::process::id() as libc::pid_t, libc::SIGINT);
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .expect("shutdown future must resolve on SIGINT")
            .expect("shutdown future must not panic");
    }
}
