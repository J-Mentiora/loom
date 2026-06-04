//! `connection_handler` — see crate root.
pub mod connection_handler;
pub use connection_handler::*;

pub mod health_rate_limit;

#[cfg(test)]
mod interface_tests;

use crate::auth_middleware::auth_middleware::{AuthError, HELLO_IDLE_TIMEOUT};
use crate::error_translator::error_translator::{JsonRpcError, LoomErrorCode};
use crate::frame_handler::frame_handler::{FrameHandler, FramedUnixStream};
use crate::schema_validator::schema_validator::ValidationOutcome;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::net::UnixStream;
use tokio_util::sync::CancellationToken;

/// Per-connection cancel registry: maps the JSON-RPC envelope `id`
/// (stringified) to a `CancellationToken` for the corresponding
/// in-flight dispatch. The `request.cancel` RPC looks up the target id
/// and fires its token; the in-flight `handle_request` races the token
/// against `router.dispatch` and returns `request-cancelled` if the
/// token wins.
///
/// Lifetime: per-connection. Dropped when the connection task exits, so
/// there's no cross-connection registry maintenance.
type ConnectionCancelRegistry = Arc<Mutex<HashMap<String, CancellationToken>>>;

/// Per-connection rate-limit bucket for `daemon.health`.
/// Lifetime: per-connection. Dropped when the connection task exits,
/// so a banned client (one that exhausts the bucket) gets its budget
/// reset only by reconnecting, which is bounded by auth.
type ConnectionHealthLimiter = Arc<Mutex<health_rate_limit::TokenBucket>>;

/// Stringify a JSON-RPC `id` (which can be string, number, or null) so
/// it's usable as a HashMap key. Treats `null` as `""` — a server
/// never assigns null to a request, but defensive in case a client
/// sends it (cancel-by-null is a no-op on lookup).
fn id_key(id: &serde_json::Value) -> String {
    match id {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

/// Per-request server-side deadline. Wraps `router.dispatch` so a hung
/// shim or stuck dispatcher can't hold the connection task indefinitely
/// (the prior code only had the 300 s socket idle timeout, which let
/// individual requests stall for up to 5 minutes before the connection
/// was reaped). Configurable via `LOOM_REQUEST_TIMEOUT_MS`; default 30 s.
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 30_000;

fn request_timeout() -> Duration {
    static CACHED: OnceLock<Duration> = OnceLock::new();
    *CACHED.get_or_init(|| {
        let ms = std::env::var("LOOM_REQUEST_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS);
        Duration::from_millis(ms)
    })
}

/// Idle timeout for the `Authenticated` state. Defaults to
/// [`AUTHENTICATED_IDLE_TIMEOUT`] (300 s); overridable via
/// `LOOM_AUTHENTICATED_IDLE_TIMEOUT_MS`. The override exists so integration
/// tests can force a fast idle-drop to exercise client reconnect; production
/// behaviour is unchanged when the env var is unset.
fn authenticated_idle_timeout() -> Duration {
    static CACHED: OnceLock<Duration> = OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("LOOM_AUTHENTICATED_IDLE_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|&ms| ms > 0)
            .map(Duration::from_millis)
            .unwrap_or(AUTHENTICATED_IDLE_TIMEOUT)
    })
}

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

        // Per-connection cancel registry. Dropped when this task exits,
        // so all in-flight tokens are released cleanly.
        let cancels: ConnectionCancelRegistry = Arc::new(Mutex::new(HashMap::new()));

        // Per-connection rate limiter for `daemon.health` (#58). Bucket
        // is dropped with this task so a misbehaving client can be
        // disconnected and lose its accumulated budget. The mutex is
        // `parking_lot` (no async yield required for read-modify-write
        // since `try_consume` is wall-clock arithmetic, no I/O).
        let health_limiter: ConnectionHealthLimiter =
            Arc::new(Mutex::new(health_rate_limit::TokenBucket::for_health()));

        // Authenticated: request dispatch loop
        loop {
            let frame =
                match tokio::time::timeout(authenticated_idle_timeout(), framed.next()).await {
                    Ok(Some(Ok(f))) => f,
                    _ => break,
                };
            let response = handle_request(&frame, &deps, &cancels, &health_limiter).await;
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
pub(crate) fn unwrap_sdk_envelope(params: serde_json::Value) -> serde_json::Value {
    let obj = match params.as_object() {
        Some(o) => o,
        None => return params,
    };
    let session_id = match obj.get("session_id") {
        Some(s) => s.clone(),
        None => return params,
    };
    // Probe for envelope shape — `action.payload`. Absent is normal
    // (every flat-shape caller passes through here); present-but-malformed
    // is a real bug worth surfacing.
    let action = match obj.get("action").and_then(|a| a.as_object()) {
        Some(a) => a,
        None => return params,
    };
    let payload_bytes = match action.get("payload").and_then(|p| p.as_array()) {
        Some(arr) => arr,
        None => return params,
    };
    let bytes: Vec<u8> = payload_bytes
        .iter()
        .filter_map(|v| v.as_u64().map(|n| n as u8))
        .collect();
    let inner_str = match std::str::from_utf8(&bytes) {
        Ok(s) => s,
        Err(_) => {
            tracing::warn!(
                "SDK envelope: action.payload bytes are not valid UTF-8 — \
                 passing params through unchanged; validator will reject"
            );
            return params;
        }
    };
    let inner: serde_json::Value = match serde_json::from_str(inner_str) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "SDK envelope: action.payload is not valid JSON — \
                 passing params through unchanged; validator will reject"
            );
            return params;
        }
    };
    let mut merged = match inner.as_object().cloned() {
        Some(m) => m,
        None => {
            tracing::warn!(
                "SDK envelope: action.payload decoded to a non-object — \
                 passing params through unchanged; validator will reject"
            );
            return params;
        }
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

async fn handle_request(
    frame: &[u8],
    deps: &ConnectionHandlerDeps,
    cancels: &ConnectionCancelRegistry,
    health_limiter: &ConnectionHealthLimiter,
) -> Vec<u8> {
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

    // Special-case: `request.cancel` is connection-scoped, not session-
    // scoped, so it bypasses the router and acts directly on the
    // per-connection registry. Params: `{"request_id": <jsonrpc-id>}`
    // where `request_id` is the JSON-RPC envelope `id` of the in-flight
    // call to cancel. Idempotent — cancelling an unknown id returns ok.
    if method == "request.cancel" {
        let target = raw_params
            .get("request_id")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let key = id_key(&target);
        let cancelled = cancels
            .lock()
            .get(&key)
            .map(|tok| {
                tok.cancel();
                true
            })
            .unwrap_or(false);
        let envelope = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "cancelled": cancelled },
        });
        return serde_json::to_vec(&envelope).unwrap_or_default();
    }

    // Resolve method aliases (e.g. SDK's `action.web.navigate` → canonical
    // `web.navigate`) and unwrap the SDK envelope shape so the validator
    // sees the flat-params shape its schemas were authored against.
    let canonical_method = loom_shared::action_aliases::canonicalise(method);
    let params = unwrap_sdk_envelope(raw_params);

    // Per-connection rate limit for `daemon.health` (#58). The
    // `{deep:true}` form fans out one CBOR probe per running shim per
    // call, so spam-probing amplifies load N× per request — see issue
    // #58 / Sec-5 of #56's security council review. Token bucket caps
    // sustained call rate (LOOM_DAEMON_HEALTH_RATE_RPS, default 10)
    // with burst tolerance (LOOM_DAEMON_HEALTH_RATE_BURST, default 30).
    // Gate is on the canonical method so SDK aliases route correctly.
    // Other RPCs bypass — auth, schema, and per-request timeout still
    // apply to all methods as before.
    if canonical_method == "daemon.health" && !health_limiter.lock().try_consume() {
        tracing::warn!(
            metric = "loom_daemon_health_rate_limited",
            request_id = %id_key(&id),
        );
        return jsonrpc_err(
            id,
            LoomErrorCode::TooManyRequests,
            "daemon.health rate limit exceeded for this connection; \
             back off and retry",
        );
    }

    match deps.validator.validate_request(canonical_method, &params) {
        ValidationOutcome::Pass => {}
        ValidationOutcome::Violation(err) | ValidationOutcome::MethodNotFound(err) => {
            return jsonrpc_error_envelope(id, &err);
        }
    }

    // Install a per-request cancellation token under this id so a
    // sibling `request.cancel` on the same connection can cooperatively
    // abort the dispatch. Removed on response (success, timeout, or
    // cancel) so the registry stays bounded by in-flight count.
    let token = CancellationToken::new();
    let key = id_key(&id);
    if !key.is_empty() {
        cancels.lock().insert(key.clone(), token.clone());
    }

    let raw = tokio::select! {
        // Cooperative cancel — `request.cancel` won the race.
        _ = token.cancelled() => {
            if !key.is_empty() {
                cancels.lock().remove(&key);
            }
            tracing::info!(
                metric = "loom_daemon_request_cancelled",
                method = canonical_method,
                request_id = %key,
                reason = "client",
            );
            return jsonrpc_err(
                id,
                LoomErrorCode::RequestCancelled,
                "request cancelled by client",
            );
        }
        // Server-side deadline.
        _ = tokio::time::sleep(request_timeout()) => {
            if !key.is_empty() {
                cancels.lock().remove(&key);
            }
            tracing::warn!(
                metric = "loom_daemon_request_timeout",
                method = canonical_method,
                request_id = %key,
                timeout_ms = request_timeout().as_millis() as u64,
            );
            return jsonrpc_err(
                id,
                LoomErrorCode::RequestTimeout,
                "request exceeded server-side per-call deadline",
            );
        }
        // Normal completion.
        bytes = deps.router.dispatch(canonical_method, params) => {
            if !key.is_empty() {
                cancels.lock().remove(&key);
            }
            bytes
        }
    };
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
