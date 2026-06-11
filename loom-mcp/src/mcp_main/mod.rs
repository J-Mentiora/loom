pub mod mcp_main;
pub use mcp_main::*;

#[cfg(test)]
mod interface_tests;

use crate::mcp_dispatcher::McpDispatcher;
use crate::mcp_observability::McpObservability;
use crate::resource_tracker::ResourceTracker;
use crate::rpc_client::{RpcClient, RpcClientConfig};
use crate::stdio_transport::{Dispatch, McpResponse, StdioTransport};
use crate::tool_cache::ToolCache;
use loom_rpc::error::LoomError;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;

pub fn config_from_args(args: &ServeArgs) -> RpcClientConfig {
    let mut cfg = RpcClientConfig::defaults();
    if let Some(path) = &args.hello_token_path {
        cfg.hello_token_path = path.clone();
    }
    if let Some(path) = &args.socket_path {
        cfg.socket_path = path.clone();
    }
    cfg
}

pub async fn run(args: ServeArgs) -> Result<(), LoomError> {
    let redact = !args.no_vault_redaction;
    let obs = crate::mcp_observability::init_subscriber(redact);
    let cfg = config_from_args(&args);
    let rpc = RpcClient::new(cfg, obs.clone());
    let shutdown = CancellationToken::new();
    install_signal_handler(shutdown.clone(), obs.clone())?;
    install_panic_hook(obs.clone());
    // Implicit-session determinism knobs (LOOM_MCP_SESSION_SEED /
    // LOOM_MCP_SESSION_CLOCK_ANCHOR / LOOM_MCP_SESSION_PROFILE). A
    // malformed value fails startup loudly — silently dropping a seed
    // would yield non-deterministic captures that look deterministic.
    let session_options = crate::mcp_dispatcher::SessionOptions::from_env()?;
    let dispatcher =
        build_dispatcher(rpc.clone(), obs.clone(), shutdown.clone(), session_options).await;
    // Re-prime the tool cache after EVERY successful (re)connect — registered
    // BEFORE the initial connect so the hook covers all of them. Without it,
    // a daemon that was down at launch (typical when the MCP client
    // autostarts at login) left `tools/list` empty forever, even after the
    // keepalive reconnected.
    install_tool_cache_reprime(&rpc, dispatcher.clone(), obs.clone()).await;
    // Attempt initial connect; proceed even if daemon is down (graceful degradation).
    let _ = rpc.connect().await;
    // Keepalive: a long-lived MCP server would otherwise sit idle and the daemon
    // drops authenticated connections after AUTHENTICATED_IDLE_TIMEOUT (default
    // 300s) — the next web action would then broken-pipe. A periodic cheap
    // `health.ping` keeps the connection warm and lets `RpcClient::call` notice
    // a drop proactively (reconnect-first). Failures route through the reconnect
    // path inside `ping()`; they never tear down the MCP session.
    install_keepalive(rpc.clone(), obs.clone());
    // prime the tool cache from the daemon's
    // `rpc.schemas` snapshot so `tools/list` returns the real method
    // set. Failure is non-fatal — the dispatcher gracefully degrades
    // to an empty list if the daemon is unreachable. Log to stderr
    // so the operator can diagnose first-boot prime failures (the
    // alternative — silent empty tools/list — was the original bug).
    if let Err(e) = dispatcher.prime_tool_cache().await {
        eprintln!("loom-mcp: tool_cache prime failed: {e}");
    }
    let dispatch = {
        let d = dispatcher.clone();
        Arc::new(move |req| {
            let d2 = d.clone();
            Box::pin(async move { d2.dispatch(req).await })
                as futures::future::BoxFuture<'static, Option<McpResponse>>
        })
    };
    let transport = StdioTransport::stdio();
    let result = serve_until_shutdown(transport, dispatch, shutdown).await;
    // Best-effort: close the implicit session created on first tool
    // call so the daemon-side WAL gets a clean SessionTerminal entry
    // and the underlying chromium subprocess gets reaped via
    // `LocalSessionManager::close`. Daemon-down paths swallow the
    // error. Bounded by SHUTDOWN_DRAIN_TIMEOUT so a hung daemon can't
    // stall process exit past the drain window.
    let _ = tokio::time::timeout(SHUTDOWN_DRAIN_TIMEOUT, dispatcher.close_implicit_session()).await;
    result
}

/// Drive the stdio loop until stdin EOF **or** until `shutdown` is
/// cancelled — by the signal task (`install_signal_handler`) or the MCP
/// `shutdown` method (`McpDispatcher::shutdown`). Both exits fall
/// through to `run`'s teardown, so `kill <pid>` closes the implicit
/// session as cleanly as a client disconnect. Split out of `run` so
/// tests can exercise the shutdown race without a real stdin or daemon.
pub async fn serve_until_shutdown<R, W>(
    transport: StdioTransport<R, W>,
    dispatch: Dispatch,
    shutdown: CancellationToken,
) -> Result<(), LoomError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    tokio::select! {
        result = transport.run(dispatch) => result,
        () = shutdown.cancelled() => Ok(()),
    }
}

/// Spawn the signal task: on SIGINT/SIGTERM, cancel `shutdown` so
/// `serve_until_shutdown` unwinds through the same teardown as stdin
/// EOF. Registering tokio handlers replaces the default kill
/// disposition, so the cancel MUST actually terminate the server — the
/// previous shape (store into a flag nothing read) left the process
/// unkillable except by SIGKILL, leaking the implicit session.
pub fn install_signal_handler(
    shutdown: CancellationToken,
    obs: Arc<McpObservability>,
) -> Result<(), LoomError> {
    tokio::spawn(async move {
        signal_wait().await;
        obs.info("shutdown signal received", serde_json::json!({}));
        shutdown.cancel();
    });
    Ok(())
}

pub async fn build_dispatcher(
    rpc: Arc<RpcClient>,
    obs: Arc<McpObservability>,
    shutdown: CancellationToken,
    session_options: crate::mcp_dispatcher::SessionOptions,
) -> Arc<McpDispatcher> {
    let tool_cache = ToolCache::new(rpc.clone());
    let resource_tracker = ResourceTracker::new(rpc.clone());
    McpDispatcher::new(
        tool_cache,
        resource_tracker,
        rpc,
        obs,
        shutdown,
        session_options,
    )
}

/// Register the `on_connected` hook that re-primes the tool cache after
/// every successful (re)connect. All reconnect paths (keepalive ping, the
/// call-path reconnect-first/retry, the backoff reconnect task) converge on
/// `RpcClient::connect`, which fires the registered callbacks — so a daemon
/// that comes up after launch, or restarts mid-session, repopulates
/// `tools/list` without an MCP-server restart. The prime is idempotent
/// (re-fetches and overwrites), so firing on the initial connect as well as
/// on overlapping reconnects is safe.
pub async fn install_tool_cache_reprime(
    rpc: &Arc<RpcClient>,
    dispatcher: Arc<McpDispatcher>,
    obs: Arc<McpObservability>,
) {
    rpc.register_on_connected(Arc::new(move || {
        let d = dispatcher.clone();
        let obs = obs.clone();
        // The callback fires synchronously inside `connect()`; the prime is
        // an RPC round trip, so run it on its own task.
        tokio::spawn(async move {
            if let Err(e) = d.prime_tool_cache().await {
                obs.info(
                    "tool_cache_reprime_failed",
                    serde_json::json!({ "code": e.code.as_wire() }),
                );
            }
        });
    }))
    .await;
}

/// Keepalive interval. Kept well under the daemon's 300s authenticated idle
/// timeout (≤ ⅓ of the window, so a single missed beat still can't trigger a
/// drop). Overridable via `LOOM_MCP_KEEPALIVE_SECS` for tests.
fn keepalive_interval() -> std::time::Duration {
    let secs = std::env::var("LOOM_MCP_KEEPALIVE_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(60);
    std::time::Duration::from_secs(secs)
}

/// Spawn the background keepalive task. Ticks `health.ping` on an interval;
/// errors are swallowed (the reconnect path inside `ping()`/`call()` handles
/// recovery) and never affect the stdio request loop.
pub fn install_keepalive(rpc: Arc<RpcClient>, obs: Arc<McpObservability>) {
    let interval = keepalive_interval();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // Skip the immediate first tick (we just connected).
        ticker.tick().await;
        loop {
            ticker.tick().await;
            if let Err(e) = rpc.ping().await {
                obs.info(
                    "keepalive_miss",
                    serde_json::json!({ "code": e.code.as_wire() }),
                );
            }
        }
    });
}

pub fn install_panic_hook(_obs: Arc<McpObservability>) {
    std::panic::set_hook(Box::new(|info| {
        tracing::error!(panic = %info, "task panicked");
    }));
}

#[cfg(unix)]
async fn signal_wait() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut sigterm = signal(SignalKind::terminate()).unwrap_or_else(|_| {
        // If SIGTERM registration fails, fall back to ctrl_c only.
        // We return a dummy stream that never fires by re-registering SIGINT.
        signal(SignalKind::interrupt()).unwrap()
    });
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = sigterm.recv() => {}
    }
}

#[cfg(not(unix))]
async fn signal_wait() {
    let _ = tokio::signal::ctrl_c().await;
}
