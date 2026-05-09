//! `connection_handler` — see crate root.
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

/// Unwrap the @mentiora-ai/loom-sdk@0.9.x envelope shape:
///
///   { "session_id": "...",
///     "action": { "kind": "navigate", "payload": [<bytes>], "deadline_ms": N } }
///
/// into the flat shape the validator schemas expect:
///
///   { "session_id": "...", "url": "..." }
///
/// `payload` is a JSON-encoded byte array (UTF-8). Decode + merge its
/// fields into the top-level object alongside `session_id`. If the
/// input doesn't look like an envelope (no `action.payload`) it's
/// returned unchanged so historical flat-shape callers (CLI, MCP,
/// older SDKs) still work.
fn unwrap_sdk_envelope(params: serde_json::Value) -> serde_json::Value {
    let obj = match params.as_object() {
        Some(o) => o,
        None => return params,
    };
    let session_id = match obj.get("session_id") {
        Some(s) => s.clone(),
        None => return params,
    };
    let payload_bytes = obj
        .get("action")
        .and_then(|a| a.as_object())
        .and_then(|a| a.get("payload"))
        .and_then(|p| p.as_array());
    let payload_bytes = match payload_bytes {
        Some(arr) => arr,
        None => return params,
    };
    let bytes: Vec<u8> = payload_bytes
        .iter()
        .filter_map(|v| v.as_u64().map(|n| n as u8))
        .collect();
    let inner_str = match std::str::from_utf8(&bytes) {
        Ok(s) => s,
        Err(_) => return params,
    };
    let inner: serde_json::Value = match serde_json::from_str(inner_str) {
        Ok(v) => v,
        Err(_) => return params,
    };
    let mut merged = match inner.as_object().cloned() {
        Some(m) => m,
        None => return params,
    };
    // The wire/registered schemas use `session` (singular) as the key
    // name, while the SDK and the router-level `session_id_from_params`
    // helper both speak `session_id`. We emit `session` here so the
    // validator's `additionalProperties:false` + `required:["session"]`
    // rules pass; `session_id_from_params` already accepts either form
    // at dispatch time.
    merged.insert("session".to_string(), session_id);
    serde_json::Value::Object(merged)
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

    let raw_params = request
        .get("params")
        .cloned()
        .unwrap_or(serde_json::Value::Object(Default::default()));

    // Resolve method aliases (e.g. SDK's `action.web.navigate` → canonical
    // `web.navigate`) and unwrap the SDK envelope shape so the validator
    // sees the flat-params shape its schemas were authored against.
    let canonical_method = loom_shared::action_aliases::canonicalise(method);
    let params = unwrap_sdk_envelope(raw_params);

    match deps.validator.validate_request(canonical_method, &params) {
        ValidationOutcome::Pass => {}
        ValidationOutcome::Violation(err) | ValidationOutcome::MethodNotFound(err) => {
            return jsonrpc_error_envelope(id, &err);
        }
    }

    let raw = deps.router.dispatch(canonical_method, params).await;
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
