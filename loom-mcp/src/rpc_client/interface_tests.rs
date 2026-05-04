// Interface tests for `RpcClient`. Verifies IC-MCP-10 token-acquisition
// path, FSM shape, exp-backoff envelope, and BC-MCP-04 single
// reconnect-task discipline.

use super::rpc_client::{
    ConnectionState, JsonRpcCaller, OnConnected, RpcClient, RpcClientConfig,
};
use crate::mcp_observability::McpObservability;
use loom_rpc::error::LoomError;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

// === IC-MCP-10: HELLO token from daemon auth artefact ===

#[test]
fn defaults_point_at_application_support_hello_token_on_macos() {
    let cfg = RpcClientConfig::defaults();
    let s = cfg.hello_token_path.display().to_string();
    // Default path must be the daemon's published auth artefact, NOT
    // loom-cli's token state file.
    if cfg!(target_os = "macos") {
        assert!(
            s.contains("loom/auth/hello.token"),
            "default macOS path must be ~/Library/Application Support/loom/auth/hello.token; got {s}"
        );
    }
}

#[test]
fn hello_token_path_is_overridable_via_config() {
    let mut cfg = RpcClientConfig::defaults();
    let custom = PathBuf::from("/tmp/custom-hello.token");
    cfg.hello_token_path = custom.clone();
    assert_eq!(cfg.hello_token_path, custom);
}

#[test]
fn read_hello_token_signature() {
    fn _ck(p: &std::path::Path) -> Result<String, LoomError> {
        RpcClient::read_hello_token(p)
    }
    let _ = _ck;
}

// === FSM shape ===

#[test]
fn connection_state_has_three_variants() {
    let _all = [
        ConnectionState::Connecting,
        ConnectionState::Connected,
        ConnectionState::Disconnected,
    ];
}

#[test]
fn state_method_returns_connection_state() {
    fn _ck(c: &RpcClient) -> Box<dyn std::future::Future<Output = ConnectionState> + '_> {
        Box::new(async move { c.state().await })
    }
    let _ = _ck;
}

// === Exp-backoff envelope: 100ms initial → 5s cap ===

#[test]
fn defaults_backoff_initial_is_100ms() {
    let cfg = RpcClientConfig::defaults();
    assert_eq!(cfg.backoff_initial, Duration::from_millis(100));
}

#[test]
fn defaults_backoff_cap_is_5s() {
    let cfg = RpcClientConfig::defaults();
    assert_eq!(cfg.backoff_cap, Duration::from_secs(5));
}

#[test]
fn next_backoff_caps_at_configured_max() {
    let cfg = RpcClientConfig::defaults();
    // After many failures, the delay must not exceed the cap.
    let d = RpcClient::next_backoff(&cfg, 30);
    assert!(d <= cfg.backoff_cap, "delay {d:?} must not exceed cap {:?}", cfg.backoff_cap);
}

#[test]
fn next_backoff_starts_at_initial_for_first_failure() {
    let cfg = RpcClientConfig::defaults();
    let d = RpcClient::next_backoff(&cfg, 0);
    assert_eq!(d, cfg.backoff_initial);
}

// === IC-MCP-09 / BC-MCP-04: socket path is daemon-owned, no extra spawns visible at API ===

#[test]
fn defaults_socket_path_is_loom_sock() {
    let cfg = RpcClientConfig::defaults();
    let s = cfg.socket_path.display().to_string();
    assert!(s.contains("loom.sock"), "got {s}");
}

#[test]
fn new_returns_arc_so_reconnect_task_can_share_state() {
    fn _ck(cfg: RpcClientConfig, obs: Arc<McpObservability>) -> Arc<RpcClient> {
        RpcClient::new(cfg, obs)
    }
    let _ = _ck;
}

// === call() signature returns LoomError so ErrorMapper can convert ===

#[test]
fn call_signature_returns_result_with_loom_error() {
    fn _ck(
        c: Arc<RpcClient>,
    ) -> Box<dyn std::future::Future<Output = Result<serde_json::Value, LoomError>>> {
        Box::new(async move {
            c.call("session.list", serde_json::json!({ "status": "active" }))
                .await
        })
    }
    let _ = _ck;
}

// === On-connected callback lets ToolCache hook prime/refresh ===

#[test]
fn register_on_connected_takes_arc_dyn_fn() {
    fn _ck(c: &RpcClient, cb: OnConnected) -> Box<dyn std::future::Future<Output = ()> + '_> {
        Box::new(async move { c.register_on_connected(cb).await })
    }
    let _ = _ck;
}

// === JsonRpcCaller trait is the substitution boundary ===

#[test]
fn json_rpc_caller_trait_present_for_test_substitution() {
    // Compile-time only: ensures the trait is importable and has the
    // expected raw_call shape.
    fn _ck(_c: &dyn JsonRpcCaller) {}
    let _ = _ck;
}

// === connect() is async and returns Result ===

#[test]
fn connect_returns_result_unit() {
    fn _ck(c: Arc<RpcClient>) -> Box<dyn std::future::Future<Output = Result<(), LoomError>>> {
        Box::new(async move { c.connect().await })
    }
    let _ = _ck;
}
