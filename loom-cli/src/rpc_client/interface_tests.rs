// Interface tests for `RpcClient`. Verifies socket-path
// shape, 1:1 error mapping signatures, and the retry policy
// defaults.

use super::rpc_client::{RpcClient, RpcClientConfig, RpcError};
use crate::CliError;
use std::time::Duration;

#[test]
fn rpc_client_config_carries_socket_path_auth_dir_and_timeout() {
    let c = RpcClientConfig {
        socket_path: "/tmp/loom.sock".into(),
        auth_dir: "/tmp/loom-auth".into(),
        request_timeout: Duration::from_secs(30),
    };
    assert_eq!(c.request_timeout, Duration::from_secs(30));
    assert_eq!(c.auth_dir, std::path::PathBuf::from("/tmp/loom-auth"));
}

#[test]
fn new_does_not_open_socket() {
    let c = RpcClientConfig {
        socket_path: "/tmp/nonexistent.sock".into(),
        auth_dir: "/tmp/nonexistent-auth".into(),
        request_timeout: Duration::from_millis(1),
    };
    let _client = RpcClient::new(c);
    // Constructing the client must NOT touch the socket — that's
    // `connect`'s job. If `new` did socket I/O the client would not be
    // usable in unit tests.
}

// === resolved auth_dir is honoured (regression: config-file/LOOM_AUTH_DIR
// auth_dir was silently ignored — open_and_hello hardcoded
// default_auth_paths()) ===
//
// A fake daemon socket accepts one connection and records the first
// frame. The client's `auth_dir` points at a tempdir holding a custom
// hello.token + a live daemon.pid (this test process). If the wiring
// regresses to the hardcoded default path, the HELLO either carries a
// different token or the call fails before connecting — both fail the
// assertion.
async fn read_frame_from(stream: &mut tokio::net::UnixStream) -> Vec<u8> {
    use tokio::io::AsyncReadExt as _;
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await.unwrap();
    let mut buf = vec![0u8; u32::from_be_bytes(len_buf) as usize];
    stream.read_exact(&mut buf).await.unwrap();
    buf
}

async fn write_frame_to(stream: &mut tokio::net::UnixStream, data: &[u8]) {
    use tokio::io::AsyncWriteExt as _;
    let len = (data.len() as u32).to_be_bytes();
    stream.write_all(&len).await.unwrap();
    stream.write_all(data).await.unwrap();
}

/// Tempdir auth fixture + a fake daemon socket. Returns (tempdir guard,
/// socket_path, auth_dir, listener).
fn fake_daemon_fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    tokio::net::UnixListener,
) {
    let dir = tempfile::tempdir().unwrap();
    let auth_dir = dir.path().join("custom-auth");
    std::fs::create_dir_all(&auth_dir).unwrap();
    std::fs::write(auth_dir.join("hello.token"), "tok-from-custom-dir\n").unwrap();
    // PID of this test process — alive, so read_hello_token proceeds.
    std::fs::write(auth_dir.join("daemon.pid"), std::process::id().to_string()).unwrap();
    let socket_path = dir.path().join("fake-daemon.sock");
    let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
    (dir, socket_path, auth_dir, listener)
}

#[tokio::test]
async fn connect_reads_hello_token_from_configured_auth_dir() {
    let (_dir, socket_path, auth_dir, listener) = fake_daemon_fixture();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let hello = read_frame_from(&mut stream).await;
        // Consume the pipelined daemon.hello probe and ack it like a
        // new daemon, so connect() resolves on the positive-ack path.
        let _probe = read_frame_from(&mut stream).await;
        write_frame_to(
            &mut stream,
            br#"{"jsonrpc":"2.0","id":0,"result":{"hello":"ok","server":"test"}}"#,
        )
        .await;
        String::from_utf8(hello).unwrap()
    });

    let client = RpcClient::new(RpcClientConfig {
        socket_path,
        auth_dir,
        request_timeout: Duration::from_secs(1),
    });
    client.connect().await.expect("connect must succeed");

    let hello = server.await.unwrap();
    assert_eq!(
        hello, "HELLO tok-from-custom-dir",
        "HELLO must carry the token from the configured auth_dir"
    );
}

// === HELLO ack handshake (client side) ===

/// new-client ↔ old-daemon (≤0.10.x): the daemon doesn't know
/// `daemon.hello` and answers a normal `method_not_found` envelope —
/// which proves auth passed. connect() must succeed, with no timeout
/// stall in either direction.
#[tokio::test]
async fn connect_accepts_method_not_found_probe_reply_from_old_daemon() {
    let (_dir, socket_path, auth_dir, listener) = fake_daemon_fixture();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _hello = read_frame_from(&mut stream).await;
        let _probe = read_frame_from(&mut stream).await;
        write_frame_to(
            &mut stream,
            br#"{"jsonrpc":"2.0","id":0,"error":{"code":"method_not_found","message":"method not found: daemon.hello"}}"#,
        )
        .await;
        // Hold open so the client doesn't observe a premature EOF.
        tokio::time::sleep(Duration::from_millis(100)).await;
    });

    let client = RpcClient::new(RpcClientConfig {
        socket_path,
        auth_dir,
        request_timeout: Duration::from_secs(1),
    });
    client
        .connect()
        .await
        .expect("old-daemon method_not_found reply must count as authenticated");
    server.await.unwrap();
}

/// Auth rejection: the daemon sends its bare typed JsonRpcError (no
/// envelope keys) and closes — connect() must surface AuthFailed, not
/// parse it as a response.
#[tokio::test]
async fn connect_maps_bare_error_frame_to_auth_failed() {
    let (_dir, socket_path, auth_dir, listener) = fake_daemon_fixture();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _hello = read_frame_from(&mut stream).await;
        write_frame_to(
            &mut stream,
            br#"{"code":"protocol_auth_required","message":"authentication required"}"#,
        )
        .await;
        // Close immediately, like the real daemon.
    });

    let client = RpcClient::new(RpcClientConfig {
        socket_path,
        auth_dir,
        request_timeout: Duration::from_secs(1),
    });
    let err = client.connect().await.expect_err("must fail auth");
    assert!(
        matches!(
            err,
            CliError::Connection(crate::error_mapper::ConnectionError::AuthFailed)
        ),
        "bare error frame must map to AuthFailed; got: {err:?}"
    );
    server.await.unwrap();
}

/// Close-without-ack (rejection where the error frame was lost): EOF
/// before any probe reply is auth failure, not success.
#[tokio::test]
async fn connect_maps_close_without_ack_to_auth_failed() {
    let (_dir, socket_path, auth_dir, listener) = fake_daemon_fixture();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _hello = read_frame_from(&mut stream).await;
        drop(stream);
    });

    let client = RpcClient::new(RpcClientConfig {
        socket_path,
        auth_dir,
        request_timeout: Duration::from_secs(1),
    });
    let err = client.connect().await.expect_err("must fail");
    assert!(
        matches!(
            err,
            CliError::Connection(crate::error_mapper::ConnectionError::AuthFailed)
        ),
        "close without ack must map to AuthFailed; got: {err:?}"
    );
    server.await.unwrap();
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

// === From<RpcError> for CliError mirrors LoomErrorCode 1:1 ===
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
