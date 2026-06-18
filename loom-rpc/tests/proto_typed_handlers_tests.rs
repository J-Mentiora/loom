//! Token handshake required.
//!
//! Given the daemon configured with auth token T,
//! When a JSON-RPC client connects but does not issue HELLO {token: T}
//! as the first message,
//! Then the server closes the connection after a typed error frame
//! `{code: "protocol_auth_required"}` and no methods are dispatched.

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use loom_rpc::{
    auth_middleware::auth_middleware::Token,
    frame_handler::frame_handler::FrameHandler,
    socket_server::{SocketServer, SocketServerConfig},
};
use std::sync::Arc;
use tempfile::tempdir;
use tokio::net::UnixStream;

mod common;

type FramedClient = loom_rpc::frame_handler::frame_handler::FramedUnixStream;

async fn read_json(framed: &mut FramedClient) -> serde_json::Value {
    let bytes = framed
        .next()
        .await
        .expect("expected a frame")
        .expect("frame read must not error");
    serde_json::from_slice(&bytes).expect("frame must be valid JSON")
}

async fn write_frame(framed: &mut FramedClient, data: &[u8]) {
    framed
        .send(Bytes::copy_from_slice(data))
        .await
        .expect("frame write must succeed");
}

#[tokio::test]
async fn connection_closed_when_no_hello_sent() {
    // Serialize the process-global umask window in try_bind across tempdir+bind; dropped
    // before the first `.await` so the future stays Send (see common::BIND_LOCK).
    let bind_lock = common::bind_guard();
    let dir = tempdir().unwrap();
    let socket_path = dir.path().join("loom.sock");
    let config = SocketServerConfig {
        socket_path: socket_path.clone(),
        token_override: None,
    };
    let deps = Arc::new(common::test_handler_deps());
    let server = SocketServer::new(config, Arc::clone(&deps)).unwrap();
    drop(bind_lock);

    let handle = tokio::runtime::Handle::current();
    let server_task =
        tokio::spawn(async move { server.serve(handle, futures::future::pending::<()>()).await });

    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    let client = UnixStream::connect(&socket_path).await.unwrap();
    let mut framed = FrameHandler::wrap_stream(client);

    // Send nothing — server must close after HELLO_IDLE_TIMEOUT (5s).
    let result = tokio::time::timeout(std::time::Duration::from_secs(6), framed.next())
        .await
        .expect("server must close unauthenticated connection within 6s (HELLO_IDLE_TIMEOUT=5s)");

    match result {
        Some(Ok(bytes)) => {
            let val: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                val["code"].as_str().unwrap_or(""),
                "protocol_auth_required",
                "must send protocol_auth_required on timeout"
            );
        }
        Some(Err(_)) | None => { /* connection reset or EOF is also correct */ }
    }

    server_task.abort();
}

#[tokio::test]
async fn connection_closed_when_wrong_token_sent() {
    // Serialize the process-global umask window in try_bind across tempdir+bind; dropped
    // before the first `.await` so the future stays Send (see common::BIND_LOCK).
    let bind_lock = common::bind_guard();
    let dir = tempdir().unwrap();
    let socket_path = dir.path().join("loom.sock");
    let config = SocketServerConfig {
        socket_path: socket_path.clone(),
        token_override: Some(Token("correct-secret-token".to_string())),
    };
    let deps = Arc::new(common::test_handler_deps());
    let server = SocketServer::new(config, Arc::clone(&deps)).unwrap();
    drop(bind_lock);

    let handle = tokio::runtime::Handle::current();
    let server_task =
        tokio::spawn(async move { server.serve(handle, futures::future::pending::<()>()).await });

    tokio::time::sleep(std::time::Duration::from_millis(30)).await;

    let client = UnixStream::connect(&socket_path).await.unwrap();
    let mut framed = FrameHandler::wrap_stream(client);

    // Send HELLO with wrong token.
    write_frame(&mut framed, b"HELLO wrong-token-value").await;

    // Expect protocol_auth_required error frame.
    let response = tokio::time::timeout(std::time::Duration::from_secs(2), read_json(&mut framed))
        .await
        .expect("server must respond within 2s after wrong HELLO");

    assert_eq!(
        response["code"].as_str().unwrap_or(""),
        "protocol_auth_required",
        "wrong token must yield protocol_auth_required"
    );

    // Connection must close after auth failure.
    let next = tokio::time::timeout(std::time::Duration::from_secs(1), framed.next())
        .await
        .expect("server must close connection promptly after auth failure");
    assert!(
        next.map(|r| r.is_err()).unwrap_or(true),
        "connection must be closed after auth failure"
    );

    server_task.abort();
}
