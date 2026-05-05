//! `connection_handler` — re-exports the implementation submodule.
pub mod connection_handler;
pub use connection_handler::*;

#[cfg(test)]
mod interface_tests;

use crate::auth_middleware::auth_middleware::{AuthError, HELLO_IDLE_TIMEOUT};
use crate::error_translator::error_translator::{JsonRpcError, LoomErrorCode};
use crate::frame_handler::frame_handler::{FrameHandler, FramedUnixStream};
use crate::schema_validator::schema_validator::ValidationOutcome;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::net::UnixStream;

impl ConnectionHandler {
    pub fn new(deps: Arc<ConnectionHandlerDeps>) -> Self {
        Self {
            deps,
            state: ConnectionState::AwaitingHello,
        }
    }

    /// Run the AwaitingHello → Authenticated FSM for one connection.
    pub async fn run(self, stream: UnixStream) {
        let deps = self.deps;
        let mut framed = FrameHandler::wrap_stream(stream);

        // AwaitingHello: read first frame within HELLO_IDLE_TIMEOUT
        let frame = match tokio::time::timeout(HELLO_IDLE_TIMEOUT, framed.next()).await {
            Ok(Some(Ok(f))) => f,
            _ => {
                let err = deps.auth.auth_error_to_envelope(AuthError::Timeout);
                let _ = send_error(&mut framed, &err).await;
                return;
            }
        };

        // Parse HELLO frame
        let msg = match deps.auth.parse_hello(&frame) {
            Ok(m) => m,
            Err(e) => {
                let err = deps.auth.auth_error_to_envelope(e);
                let _ = send_error(&mut framed, &err).await;
                return;
            }
        };

        if let Err(e) = deps.auth.validate_hello(&msg) {
            let err = deps.auth.auth_error_to_envelope(e);
            let _ = send_error(&mut framed, &err).await;
            return;
        }

        // Authenticated: request dispatch loop
        loop {
            let frame = match tokio::time::timeout(AUTHENTICATED_IDLE_TIMEOUT, framed.next()).await
            {
                Ok(Some(Ok(f))) => f,
                _ => break,
            };
            let response = handle_request(&frame, &deps).await;
            if framed.send(Bytes::from(response)).await.is_err() {
                break;
            }
        }
    }
}

async fn send_error(framed: &mut FramedUnixStream, err: &JsonRpcError) -> Result<(), ()> {
    let bytes = serde_json::to_vec(err).unwrap_or_default();
    framed.send(Bytes::from(bytes)).await.map_err(|_| ())
}

async fn handle_request(frame: &[u8], deps: &ConnectionHandlerDeps) -> Vec<u8> {
    let request: serde_json::Value = match serde_json::from_slice(frame) {
        Ok(v) => v,
        Err(_) => {
            return jsonrpc_err(
                serde_json::Value::Null,
                LoomErrorCode::ProtocolMalformed,
                "invalid JSON in request frame",
            );
        }
    };

    let id = request
        .get("id")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    let method = match request["method"].as_str() {
        Some(m) => m,
        None => {
            return jsonrpc_err(
                id,
                LoomErrorCode::ProtocolMalformed,
                "request missing 'method' field",
            );
        }
    };

    let params = request
        .get("params")
        .cloned()
        .unwrap_or(serde_json::Value::Object(Default::default()));

    match deps.validator.validate_request(method, &params) {
        ValidationOutcome::Pass => {}
        ValidationOutcome::Violation(err) | ValidationOutcome::MethodNotFound(err) => {
            return jsonrpc_error_envelope(id, &err);
        }
    }

    let raw = deps.router.dispatch(method, params).await;
    // Wrap the router's raw payload in a JSON-RPC 2.0 envelope.
    // The router tags errors with `__loom_rpc_error: true` so we can
    // distinguish them from success payloads without heuristics.
    let payload: serde_json::Value =
        serde_json::from_slice(&raw).unwrap_or(serde_json::Value::Null);
    if payload.get("__loom_rpc_error").and_then(|v| v.as_bool()) == Some(true) {
        let mut err_obj = payload.clone();
        if let serde_json::Value::Object(ref mut m) = err_obj {
            m.remove("__loom_rpc_error");
        }
        let envelope = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": err_obj,
        });
        serde_json::to_vec(&envelope).unwrap_or_default()
    } else {
        let envelope = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": payload,
        });
        serde_json::to_vec(&envelope).unwrap_or_default()
    }
}

fn jsonrpc_err(id: serde_json::Value, code: LoomErrorCode, message: &str) -> Vec<u8> {
    let err = JsonRpcError {
        code,
        message: message.to_string(),
        data: None,
    };
    jsonrpc_error_envelope(id, &err)
}

fn jsonrpc_error_envelope(id: serde_json::Value, err: &JsonRpcError) -> Vec<u8> {
    let envelope = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": err,
    });
    serde_json::to_vec(&envelope).unwrap_or_default()
}
