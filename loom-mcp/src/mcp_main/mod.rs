pub mod mcp_main;
pub use mcp_main::*;

#[cfg(test)]
mod interface_tests;

use crate::mcp_dispatcher::McpDispatcher;
use crate::mcp_observability::McpObservability;
use crate::resource_tracker::ResourceTracker;
use crate::rpc_client::{RpcClient, RpcClientConfig};
use crate::stdio_transport::{McpResponse, StdioTransport};
use crate::tool_cache::ToolCache;
use loom_rpc::error::LoomError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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
    let shutdown_flag = Arc::new(AtomicBool::new(false));
    install_signal_handler(shutdown_flag.clone(), obs.clone())?;
    install_panic_hook(obs.clone());
    let dispatcher = build_dispatcher(rpc.clone(), obs.clone(), shutdown_flag.clone()).await;
    // Attempt initial connect; proceed even if daemon is down (graceful degradation).
    let _ = rpc.connect().await;
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
    let result = transport.run(dispatch).await;
    // Best-effort: close the implicit session created on first tool
    // call so the daemon-side WAL gets a clean SessionTerminal entry
    // and the underlying chromium subprocess gets reaped via
    // `LocalSessionManager::close`. Daemon-down paths swallow the
    // error.
    dispatcher.close_implicit_session().await;
    result
}

pub fn install_signal_handler(
    flag: Arc<AtomicBool>,
    obs: Arc<McpObservability>,
) -> Result<(), LoomError> {
    tokio::spawn(async move {
        signal_wait().await;
        flag.store(true, Ordering::SeqCst);
        obs.info("shutdown signal received", serde_json::json!({}));
    });
    Ok(())
}

pub async fn build_dispatcher(
    rpc: Arc<RpcClient>,
    obs: Arc<McpObservability>,
    shutdown_flag: Arc<AtomicBool>,
) -> Arc<McpDispatcher> {
    let tool_cache = ToolCache::new(rpc.clone());
    let resource_tracker = ResourceTracker::new(rpc.clone());
    McpDispatcher::new(tool_cache, resource_tracker, rpc, obs, shutdown_flag)
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
