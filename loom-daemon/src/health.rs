//! Daemon health + session-shutdown bridges.
//!
//! Split out of `lib.rs` (large-file refactor), unchanged:
//!   * `WasmHostShutdownAdapter` — adapts `Arc<WasmHost>` to the
//!     `SessionShutdownAsync` trait `RpcHandlers::session_kill` calls
//!     (wraps `shutdown_session` in a per-call `tokio::time::timeout`).
//!   * `DaemonHealthBridge` — the `DaemonHealthProvider` (shallow snapshot)
//!     + `DaemonHealthAsync` (deep per-shim probe fan-out) for `daemon.health`.
//!
//! Both are constructed and wired in `async_main` in `lib.rs`.

use crate::{now_epoch_ms, reaper};
use loom_core::core_api_facade::CoreApiFacade;
use loom_rpc::rpc_handlers::rpc_handlers::{
    DaemonHealth, DaemonHealthAsync, DaemonHealthProvider, ProbeStatus, SessionShutdownAsync,
    ShimBreakerSnapshot, ShimDeepHealth,
};
use std::sync::Arc;

// ─── Bridge: WasmHost → SessionShutdownAsync ────────────────────────────────
//
// Adapts the daemon's `Arc<loom_host::WasmHost>` to the `SessionShutdownAsync`
// trait that `RpcHandlers::session_kill` calls. Wraps `host.shutdown_session`
// in a `tokio::time::timeout` set to the per-call ceiling so a wedged shim
// can't hold `session.kill` indefinitely — after the ceiling, the inner
// `shutdown_process` has already escalated to SIGKILL via its own
// SIGTERM(2s)→SIGKILL(1s) sequence at process.rs:438-457, so the timeout
// here is a belt-and-braces backstop.

pub(crate) struct WasmHostShutdownAdapter {
    pub(crate) host: Arc<loom_host::WasmHost>,
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

pub(crate) struct DaemonHealthBridge {
    pub(crate) core: Arc<CoreApiFacade>,
    pub(crate) wasm_host: Option<Arc<loom_host::WasmHost>>,
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
