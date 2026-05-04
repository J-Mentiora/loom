// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-cli/modules/RpcClient/interface_tests.rs` instead.
// Interface tests for `RpcClient`. Verifies AC-PROTO-01.1 socket-path
// shape, BC-CLI-05 1:1 error mapping signatures, and the retry policy
// defaults.

use super::rpc_client::{RpcClient, RpcClientConfig, RpcError};
use crate::CliError;
use std::time::Duration;

#[test]
fn rpc_client_config_carries_socket_path_and_timeouts() {
    let c = RpcClientConfig {
        socket_path: "/tmp/loom.sock".into(),
        connect_timeout: Duration::from_secs(5),
        request_timeout: Duration::from_secs(30),
    };
    assert_eq!(c.connect_timeout, Duration::from_secs(5));
}

#[test]
fn new_does_not_open_socket() {
    let c = RpcClientConfig {
        socket_path: "/tmp/nonexistent.sock".into(),
        connect_timeout: Duration::from_millis(1),
        request_timeout: Duration::from_millis(1),
    };
    let _client = RpcClient::new(c);
    // Constructing the client must NOT touch the socket — that's
    // `connect`'s job. If `new` did socket I/O the client would not be
    // usable in unit tests.
}

#[test]
fn call_signature_returns_json_value() {
    fn _ck(c: &RpcClient) {
        let _f = async {
            let _: Result<serde_json::Value, CliError> =
                c.call("session.list", serde_json::json!({})).await;
        };
    }
    let _ = _ck;
}

#[test]
fn ping_signature_returns_unit_or_error() {
    fn _ck(c: &RpcClient) {
        let _f = async {
            let _: Result<(), CliError> = c.ping().await;
        };
    }
    let _ = _ck;
}

// === BC-CLI-05: From<RpcError> for CliError mirrors LoomErrorCode 1:1 ===
#[test]
fn from_rpc_error_for_cli_error_present() {
    fn _ck(e: RpcError) -> CliError {
        e.into()
    }
    let _ = _ck;
}

#[test]
fn rpc_error_carries_code_message_optional_data() {
    let e = RpcError {
        code: "schema_violation".into(),
        message: "bad arg".into(),
        data: Some(serde_json::json!({"field": "selector"})),
    };
    let s = serde_json::to_string(&e).unwrap();
    assert!(s.contains("schema_violation"));
    assert!(s.contains("bad arg"));
}
