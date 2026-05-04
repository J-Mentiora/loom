//! `supervisor::run` — top-level driver for the `loom-shim-chromium`
//! binary. Wires the inherited socketpair → IpcEndpoint → ShimDispatcher
//! → ChromiumActionExecutor → ChromiumCdpConnection → real Chromium
//! subprocess via ChromiumSupervisor.
//!
//! Drives until either:
//!   - the daemon sends `ShimRequest::Shutdown`
//!   - the daemon closes the socketpair (EOF)
//!   - Chromium exits / hangs (handled via Supervisor::handle_crash)
//!
//! Cleanup on return: shutdown CDP session → SIGTERM/SIGKILL the
//! Chromium child → abort IPC tasks.

use crate::action_executor::action_executor::ChromiumActionExecutor;
use crate::cdp_connection::cdp_connection::ChromiumCdpConnection;
use crate::determinism_injector::determinism_injector::ChromiumDeterminismInjector;
use crate::dispatcher::dispatcher::{Dispatcher, ShimDispatcher};
use crate::ipc_endpoint::runner::{spawn_loops_unix, DEFAULT_CHANNEL_CAPACITY};
use crate::network_interceptor::network_interceptor::{
    parse_blocklist_with_categories, ChromiumNetworkInterceptor,
};
use crate::supervisor::supervisor::{
    ChromiumSupervisor, Supervisor, SupervisorConfig, SupervisorError,
};
use crate::target_manager::target_manager::ChromiumTargetManager;
use std::os::unix::io::{FromRawFd, RawFd};
use std::sync::Arc;
use tokio::sync::oneshot;

/// Default determinism script. Loaded from the project's assets at build
/// time via `include_str!`. The asset path is alongside the existing
/// determinism_init.js file used by the integration tests.
const DETERMINISM_SCRIPT: &str = include_str!("../../assets/determinism_init.js");

/// Default analytics/ads/telemetry blocklist (AC-DET-05.1). Embedded at
/// build time via `include_str!`. ~140 entries grouped under
/// `# --- Section ---` headers; section names become the `reason` field
/// on `BlockedEvent`s. The shim parses this once at boot.
const DEFAULT_BLOCKLIST_TEXT: &str = include_str!("../../assets/default_blocklist.txt");

/// Top-level driver. Returns Ok on graceful shutdown; Err on irrecoverable
/// startup failure (Chromium spawn / CDP connect / FD adoption).
pub async fn run(config: SupervisorConfig, ipc_fd: RawFd) -> Result<(), SupervisorError> {
    // STEP 1: adopt the inherited socketpair FD as a tokio UnixStream.
    let std_stream = unsafe { std::os::unix::net::UnixStream::from_raw_fd(ipc_fd) };
    std_stream
        .set_nonblocking(true)
        .map_err(|e| SupervisorError::SpawnFailed(format!("set_nonblocking: {e}")))?;
    let stream = tokio::net::UnixStream::from_std(std_stream)
        .map_err(|e| SupervisorError::SpawnFailed(format!("UnixStream::from_std: {e}")))?;

    let (request_rx, response_tx, ipc_handles) = spawn_loops_unix(stream, DEFAULT_CHANNEL_CAPACITY);
    let response_tx = Arc::new(response_tx);

    // STEP 2: build CDP + determinism + network + targets + executor.
    let cdp: Arc<ChromiumCdpConnection> = Arc::new(ChromiumCdpConnection::new());
    let determinism = ChromiumDeterminismInjector::new(cdp.clone(), DETERMINISM_SCRIPT.to_string());
    let targets = Arc::new(ChromiumTargetManager::new(
        cdp.clone(),
        determinism.clone(),
        response_tx.clone(),
    ));
    let blocklist = parse_blocklist_with_categories(DEFAULT_BLOCKLIST_TEXT);
    let network = ChromiumNetworkInterceptor::new_with_blocklist(cdp.clone(), blocklist);
    let executor = Arc::new(ChromiumActionExecutor::new(
        cdp.clone(),
        network,
        targets.clone(),
    ));
    let dispatcher = Arc::new(ShimDispatcher::new(
        targets.clone(),
        executor,
        response_tx.clone(),
    ));

    // STEP 3: spawn Chromium + connect CDP.
    let chromium = Arc::new(ChromiumSupervisor::new(
        config,
        cdp.clone(),
        targets,
        dispatcher.clone(),
    ));
    let _pid = chromium.start().await?;

    // STEP 4: drive the dispatcher loop until Shutdown.
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let dispatcher_for_run = dispatcher.clone();
    let dispatch_result =
        tokio::spawn(async move { dispatcher_for_run.run(request_rx, shutdown_rx).await });

    // STEP 5: wait for the dispatcher to return (Shutdown / EOF), OR
    // for SIGTERM/SIGINT — install handlers so we can reap Chromium
    // before the shim process exits, even on cooperative-but-unclean
    // termination paths (the daemon's `pkill loom-shim-chromium` after
    // a session close, for instance).
    //
    // AC-SHCRT-05.2: the binary's process group is set to the shim's
    // own pid by default (no process_group(0) in spawn_shim). Chromium's
    // pgid was explicitly set in ChromiumSupervisor::start. So a
    // `kill -9 <shim_pid>` from the host orphans Chromium unless we
    // catch SIGTERM here. SIGKILL we cannot intercept (kernel-level);
    // for that case the shim-side reap relies on the host's
    // shutdown_session forcing teardown via `loom session close`.
    let chromium_for_signal = chromium.clone();
    let signal_outcome = tokio::select! {
        biased;
        join = dispatch_result => {
            match join {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(SupervisorError::SpawnFailed(format!("dispatcher exited: {e}"))),
                Err(e) => Err(SupervisorError::SpawnFailed(format!("dispatcher join: {e}"))),
            }
        }
        sig = wait_for_term_signal() => {
            tracing::warn!("supervisor: caught signal {sig:?}, reaping Chromium");
            // Best-effort reap before we fall through to the cleanup.
            let _ = chromium_for_signal.shutdown().await;
            Ok(())
        }
    };
    let outcome = signal_outcome;

    // STEP 6: cleanup. Drop shutdown_tx so any in-flight dispatcher tasks
    // know to exit; teardown Chromium; abort IPC tasks.
    drop(shutdown_tx);
    if let Err(e) = chromium.shutdown().await {
        tracing::warn!("supervisor: chromium.shutdown failed: {e}");
    }
    ipc_handles.read.abort();
    ipc_handles.write.abort();

    outcome
}

/// Wait for SIGTERM or SIGINT and return a label. Used by `run` to
/// catch cooperative termination signals and reap Chromium before
/// process exit.
async fn wait_for_term_signal() -> &'static str {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(_) => {
            // Couldn't install handler — block forever. The dispatch
            // arm of the select! will still drive the runtime.
            std::future::pending::<()>().await;
            unreachable!();
        }
    };
    let mut int = match signal(SignalKind::interrupt()) {
        Ok(s) => s,
        Err(_) => {
            std::future::pending::<()>().await;
            unreachable!();
        }
    };
    tokio::select! {
        _ = term.recv() => "SIGTERM",
        _ = int.recv() => "SIGINT",
    }
}
