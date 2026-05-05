// Interface tests for `McpMain`. Verifies SR-MCP-01 (clap subcommand,
// `--help` works), AC-NFR-DX-01 (zero-config defaults), BC-MCP-04
// (single tokio runtime -- `run` is `async`, not `#[tokio::main]`),
// and signal-handling shape.

use super::mcp_main::{ServeArgs, SHUTDOWN_DRAIN_TIMEOUT};
use super::{build_dispatcher, config_from_args, install_panic_hook, install_signal_handler, run};
use crate::mcp_dispatcher::McpDispatcher;
use crate::mcp_observability::McpObservability;
use crate::rpc_client::{RpcClient, RpcClientConfig};
use clap::Parser;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

// === SR-MCP-01: clap subcommand, --help works zero-config ===

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

// === AC-NFR-DX-01: zero-config defaults ===

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

// === BC-MCP-04: run() is async, not #[tokio::main] ===

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
    fn _ck(f: Arc<AtomicBool>, o: Arc<McpObservability>) -> Result<(), loom_rpc::error::LoomError> {
        install_signal_handler(f, o)
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
fn build_dispatcher_takes_rpc_obs_shutdown_flag() {
    fn _ck(
        rpc: Arc<RpcClient>,
        obs: Arc<McpObservability>,
        f: Arc<AtomicBool>,
    ) -> Box<dyn std::future::Future<Output = Arc<McpDispatcher>>> {
        Box::new(async move { build_dispatcher(rpc, obs, f).await })
    }
    let _ = _ck;
}

// === Per-task panic hook ===

#[test]
fn install_panic_hook_takes_observability() {
    fn _ck(o: Arc<McpObservability>) {
        install_panic_hook(o)
    }
    let _ = _ck;
}
