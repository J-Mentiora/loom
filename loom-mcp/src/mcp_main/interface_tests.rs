// Interface tests for `McpMain`. Verifies the clap subcommand wiring
// (`--help` works zero-config), the single tokio runtime invariant
// (`run` is `async`, not `#[tokio::main]`), and signal-handling shape.

use super::mcp_main::{ServeArgs, SHUTDOWN_DRAIN_TIMEOUT};
use super::{
    build_dispatcher, config_from_args, install_panic_hook, install_signal_handler, run,
    serve_until_shutdown,
};
use crate::mcp_dispatcher::McpDispatcher;
use crate::mcp_observability::McpObservability;
use crate::rpc_client::{RpcClient, RpcClientConfig};
use crate::stdio_transport::{Dispatch, StdioTransport};
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

// === clap subcommand, --help works zero-config ===

#[test]
fn serve_args_has_clap_parser_impl() {
    // `--help` exits with 0 from clap before any data-plane work.
    // We can't test exit codes here; we check the parser is wired.
    let r = ServeArgs::try_parse_from(["serve"]);
    assert!(r.is_ok(), "default invocation must parse: {:?}", r.err());
}

#[test]
fn serve_args_help_does_not_require_daemon() {
    // `--help` is handled by clap synchronously, before `run` starts.
    let r = ServeArgs::try_parse_from(["serve", "--help"]);
    // clap returns DisplayHelp as an error variant; we just need the
    // parse to recognise the flag.
    assert!(r.is_err(), "--help must short-circuit; got Ok");
}

// === zero-config defaults ===

#[test]
fn defaults_require_no_arguments() {
    let args = ServeArgs::try_parse_from(["serve"]).unwrap();
    assert!(
        args.hello_token_path.is_none(),
        "zero-config: no token path required"
    );
    assert!(
        args.socket_path.is_none(),
        "zero-config: no socket path required"
    );
    assert!(!args.no_vault_redaction, "vault redaction default is on");
}

#[test]
fn hello_token_path_overridable_via_env() {
    let args = ServeArgs::try_parse_from(["serve", "--hello-token-path", "/tmp/h.token"]).unwrap();
    assert_eq!(args.hello_token_path, Some(PathBuf::from("/tmp/h.token")));
}

#[test]
fn socket_path_overridable() {
    let args = ServeArgs::try_parse_from(["serve", "--socket-path", "/tmp/loom.sock"]).unwrap();
    assert_eq!(args.socket_path, Some(PathBuf::from("/tmp/loom.sock")));
}

// === Config resolution: CLI > env > defaults ===

#[test]
fn config_from_args_signature() {
    fn _ck(a: &ServeArgs) -> RpcClientConfig {
        config_from_args(a)
    }
    let _ = _ck;
}

// === run() is async, not #[tokio::main] ===

#[test]
fn run_is_async_function() {
    fn _ck(
        a: ServeArgs,
    ) -> Box<dyn std::future::Future<Output = Result<(), loom_rpc::error::LoomError>>> {
        Box::new(async move { run(a).await })
    }
    let _ = _ck;
}

// === Signal handler signature ===

#[test]
fn install_signal_handler_signature() {
    fn _ck(
        t: CancellationToken,
        o: Arc<McpObservability>,
    ) -> Result<(), loom_rpc::error::LoomError> {
        install_signal_handler(t, o)
    }
    let _ = _ck;
}

// === Drain timeout pinned at 2s (per design.md §4) ===

#[test]
fn shutdown_drain_timeout_is_2s() {
    assert_eq!(SHUTDOWN_DRAIN_TIMEOUT, Duration::from_secs(2));
}

// === Dispatcher factory: lets integration tests skip the stdio loop ===

#[test]
fn build_dispatcher_takes_rpc_obs_shutdown_token() {
    fn _ck(
        rpc: Arc<RpcClient>,
        obs: Arc<McpObservability>,
        t: CancellationToken,
    ) -> Box<dyn std::future::Future<Output = Arc<McpDispatcher>>> {
        Box::new(async move { build_dispatcher(rpc, obs, t).await })
    }
    let _ = _ck;
}

// === Shutdown trigger actually terminates the stdio loop ===
// (Audit fix: signals used to store into a flag nothing read, so
// SIGTERM/Ctrl-C left the server running — unkillable except by
// SIGKILL — and the implicit session was never closed on signal.)

/// Cancelling the shutdown token must end the serve loop even while
/// stdin is still open (the signal / MCP-`shutdown` path).
#[tokio::test]
async fn serve_until_shutdown_exits_on_cancel_while_stdin_open() {
    // Keep the write half alive so the reader never sees EOF.
    let (_stdin_writer, stdin_reader) = tokio::io::duplex(64);
    let transport = StdioTransport::with_io(stdin_reader, tokio::io::sink());
    let dispatch: Dispatch = Arc::new(|_req| Box::pin(async { None }));
    let token = CancellationToken::new();
    let task = tokio::spawn(serve_until_shutdown(transport, dispatch, token.clone()));
    token.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("serve loop must exit promptly after shutdown cancel")
        .expect("serve loop must not panic");
    assert!(result.is_ok(), "shutdown exit is orderly: {result:?}");
}

/// Stdin EOF must still end the serve loop with the token uncancelled
/// (the pre-existing client-disconnect path is unchanged).
#[tokio::test]
async fn serve_until_shutdown_still_exits_on_stdin_eof() {
    let (stdin_writer, stdin_reader) = tokio::io::duplex(64);
    drop(stdin_writer); // immediate EOF
    let transport = StdioTransport::with_io(stdin_reader, tokio::io::sink());
    let dispatch: Dispatch = Arc::new(|_req| Box::pin(async { None }));
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        serve_until_shutdown(transport, dispatch, CancellationToken::new()),
    )
    .await
    .expect("serve loop must exit on stdin EOF");
    assert!(result.is_ok(), "EOF exit is orderly: {result:?}");
}

/// The MCP `shutdown` method must trigger the same shutdown the signal
/// task does: `McpDispatcher::shutdown` cancels the token the serve
/// loop selects on.
#[tokio::test]
async fn dispatcher_shutdown_cancels_serve_token() {
    let obs = McpObservability::new(true);
    let rpc = RpcClient::new(RpcClientConfig::defaults(), obs.clone());
    let token = CancellationToken::new();
    let dispatcher = build_dispatcher(rpc, obs, token.clone()).await;
    assert!(!token.is_cancelled(), "token starts uncancelled");
    dispatcher.shutdown();
    assert!(
        token.is_cancelled(),
        "MCP `shutdown` method must cancel the serve token (not a dead flag)"
    );
}

/// Signal delivery must cancel the shutdown token. Raises a real
/// SIGTERM at the test process: tokio's handler replaces the default
/// kill disposition, so the signal is observed rather than terminating
/// the test binary (workspace tests run --test-threads=1).
#[tokio::test]
async fn signal_delivery_cancels_shutdown_token() {
    let token = CancellationToken::new();
    install_signal_handler(token.clone(), McpObservability::new(true))
        .expect("install_signal_handler");
    // Let the spawned signal task poll once so the handlers are
    // registered before the signal is raised.
    tokio::time::sleep(Duration::from_millis(100)).await;
    unsafe {
        libc::kill(std::process::id() as libc::pid_t, libc::SIGTERM);
    }
    tokio::time::timeout(Duration::from_secs(5), token.cancelled())
        .await
        .expect("SIGTERM must cancel the shutdown token");
}

// === Per-task panic hook ===

#[test]
fn install_panic_hook_takes_observability() {
    fn _ck(o: Arc<McpObservability>) {
        install_panic_hook(o)
    }
    let _ = _ck;
}
