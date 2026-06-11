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
use futures::{FutureExt, SinkExt, StreamExt};
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
/// `request.cancel` is handled inline on the READ loop (a fast lane),
/// never queued behind the dispatch it is cancelling — the read loop
/// keeps polling frames while requests run on spawned tasks, so the
/// token is fired while the target dispatch is genuinely in flight.
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

/// Per-connection cap on concurrently dispatching requests. Frames are
/// read continuously (so `request.cancel` always has a fast lane), but
/// at most this many `router.dispatch` calls run at once per connection;
/// excess requests queue on the semaphore in arrival order. Configurable
/// via `LOOM_MAX_CONCURRENT_REQUESTS`; default 8.
const DEFAULT_MAX_CONCURRENT_REQUESTS: usize = 8;

fn max_concurrent_requests() -> usize {
    static CACHED: OnceLock<usize> = OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("LOOM_MAX_CONCURRENT_REQUESTS")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(DEFAULT_MAX_CONCURRENT_REQUESTS)
    })
}

/// Depth of the per-connection response queue feeding the writer task.
/// Socket writes are serialized through one writer so concurrently
/// completing dispatches can't interleave frame bytes; a full queue
/// backpressures the dispatch tasks, not the read loop.
const RESPONSE_QUEUE_DEPTH: usize = 32;

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

        // Split the framed stream: the read loop keeps polling frames
        // while requests dispatch on spawned tasks, and all socket
        // WRITES are serialized through a single writer task fed by a
        // bounded queue (responses may complete out of order; clients
        // correlate by JSON-RPC `id`).
        let (mut sink, mut frames) = framed.split();
        let (resp_tx, mut resp_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(RESPONSE_QUEUE_DEPTH);
        // Detached writer: exits when the last response sender drops or
        // the socket write fails.
        let _writer = tokio::spawn(async move {
            while let Some(response) = resp_rx.recv().await {
                if sink.send(Bytes::from(response)).await.is_err() {
                    break;
                }
            }
        });

        // Bounded per-connection dispatch concurrency. Acquired INSIDE
        // the spawned request task (racing the cancel token) so the read
        // loop never blocks on a saturated semaphore — `request.cancel`
        // frames must stay readable while requests queue.
        let dispatch_slots = Arc::new(tokio::sync::Semaphore::new(max_concurrent_requests()));

        // Authenticated: request read loop. One spawned task per request;
        // `request.cancel` is handled inline (fast lane).
        loop {
            let frame =
                match tokio::time::timeout(authenticated_idle_timeout(), frames.next()).await {
                    Ok(Some(Ok(f))) => f,
                    // Codec error — in practice a frame whose length prefix
                    // exceeds MAX_FRAME_BYTES (LengthDelimitedCodec emits an
                    // io error and cannot resync afterwards, so the
                    // connection must still close). Previously this arm
                    // silently dropped the connection and clients surfaced
                    // it as a generic "no daemon running"; send a typed
                    // error envelope and log the reason before closing.
                    Ok(Some(Err(e))) => {
                        tracing::warn!(
                            metric = "loom_daemon_frame_rejected",
                            error = %e,
                            max_frame_bytes = crate::frame_handler::frame_handler::MAX_FRAME_BYTES,
                            "closing connection: oversized or undecodable frame"
                        );
                        let response = jsonrpc_err(
                            serde_json::Value::Null,
                            LoomErrorCode::ProtocolMalformed,
                            &format!(
                                "frame rejected ({e}); the frame cap is {} bytes",
                                crate::frame_handler::frame_handler::MAX_FRAME_BYTES
                            ),
                        );
                        let _ = resp_tx.send(response).await;
                        break;
                    }
                    // Idle timeout or clean EOF.
                    _ => break,
                };

            // Parse the envelope ONCE on the read loop — needed here to
            // route the cancel fast lane and to install the cancel token
            // BEFORE the dispatch task spawns (so even a queued request
            // is cancellable).
            let request: serde_json::Value = match serde_json::from_slice(&frame) {
                Ok(v) => v,
                Err(_) => {
                    let response = jsonrpc_err(
                        serde_json::Value::Null,
                        LoomErrorCode::ProtocolMalformed,
                        "invalid JSON in request frame",
                    );
                    if resp_tx.send(response).await.is_err() {
                        break;
                    }
                    continue;
                }
            };
            let id = request
                .get("id")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let method = match request["method"].as_str() {
                Some(m) => m.to_string(),
                None => {
                    let response = jsonrpc_err(
                        id,
                        LoomErrorCode::ProtocolMalformed,
                        "request missing 'method' field",
                    );
                    if resp_tx.send(response).await.is_err() {
                        break;
                    }
                    continue;
                }
            };
            let raw_params = request
                .get("params")
                .cloned()
                .unwrap_or(serde_json::Value::Object(Default::default()));

            // FAST LANE: `request.cancel` is connection-scoped, not
            // session-scoped — it bypasses the router and acts directly
            // on the per-connection registry, handled inline so it can
            // never queue behind the request it is cancelling. Params:
            // `{"request_id": <jsonrpc-id>}` where `request_id` is the
            // JSON-RPC envelope `id` of the in-flight call to cancel.
            // Idempotent — cancelling an unknown id returns ok.
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
                let response = serde_json::to_vec(&envelope).unwrap_or_default();
                if resp_tx.send(response).await.is_err() {
                    break;
                }
                continue;
            }

            // FAST LANE: `daemon.hello` — the opt-in HELLO ack probe.
            // New clients pipeline this request right after the HELLO
            // frame; the ack `{"hello":"ok","server":<version>}` rides
            // back as its id-correlated result, giving them a positive
            // auth signal in one round trip (no 50 ms / 5 s timeout
            // heuristics).
            //
            // Wire compat, both directions:
            //   * old-client ↔ new-daemon: old clients never send this
            //     method, so the daemon never emits an unsolicited
            //     post-HELLO frame — their "any frame = rejection"
            //     timeout heuristic keeps working unchanged. The ack is
            //     ONLY ever sent when asked.
            //   * new-client ↔ old-daemon (≤0.10.x): the opt-in cannot
            //     ride the HELLO line — `parse_hello` takes the ENTIRE
            //     post-"HELLO " remainder as the token (constant-time
            //     exact compare), so any suffix would fail auth. As a
            //     separate request frame, an old daemon answers it with
            //     a normal `method_not_found` JSON-RPC envelope — which
            //     proves auth succeeded (rejections are a bare error +
            //     close) — and new clients treat that as "authenticated,
            //     pre-ack daemon".
            //
            // Answered at any point in the Authenticated state (not just
            // first-frame) and idempotent. Handled inline so the ack can
            // never queue behind a slow dispatch.
            if method == "daemon.hello" {
                let envelope = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "hello": "ok",
                        "server": env!("CARGO_PKG_VERSION"),
                    },
                });
                let response = serde_json::to_vec(&envelope).unwrap_or_default();
                if resp_tx.send(response).await.is_err() {
                    break;
                }
                continue;
            }

            // Install the per-request cancellation token under this id so
            // a sibling `request.cancel` on the same connection can
            // cooperatively abort the dispatch (even while it is queued
            // on the semaphore). Removed by the request task before its
            // response is enqueued, so the registry stays bounded by the
            // in-flight count.
            let token = CancellationToken::new();
            let key = id_key(&id);
            // A second in-flight request reusing a live id would make
            // `request.cancel` ambiguous and the responses
            // uncorrelatable — reject it outright. (Serial clients can
            // never hit this: their previous id is removed before its
            // response frame is written.) Scoped so the registry guard
            // is released before any await point.
            let duplicate_id = !key.is_empty() && {
                let mut registry = cancels.lock();
                if registry.contains_key(&key) {
                    true
                } else {
                    registry.insert(key.clone(), token.clone());
                    false
                }
            };
            if duplicate_id {
                let response = jsonrpc_err(
                    id,
                    LoomErrorCode::ProtocolMalformed,
                    "duplicate in-flight request id",
                );
                if resp_tx.send(response).await.is_err() {
                    break;
                }
                continue;
            }

            let deps = Arc::clone(&deps);
            let cancels = Arc::clone(&cancels);
            let health_limiter = Arc::clone(&health_limiter);
            let dispatch_slots = Arc::clone(&dispatch_slots);
            let resp_tx = resp_tx.clone();
            tokio::spawn(async move {
                // Wait for a dispatch slot, racing the cancel token so a
                // queued request answers `request-cancelled` immediately
                // instead of after a slot frees up.
                let _permit = tokio::select! {
                    _ = token.cancelled() => {
                        if !key.is_empty() {
                            cancels.lock().remove(&key);
                        }
                        tracing::info!(
                            metric = "loom_daemon_request_cancelled",
                            method = %method,
                            request_id = %key,
                            reason = "client",
                        );
                        let response = jsonrpc_err(
                            id,
                            LoomErrorCode::RequestCancelled,
                            "request cancelled by client",
                        );
                        let _ = resp_tx.send(response).await;
                        return;
                    }
                    permit = dispatch_slots.acquire_owned() => match permit {
                        Ok(p) => p,
                        // Semaphore closed — connection torn down.
                        Err(_) => return,
                    },
                };
                // Per-task panic isolation: a panicking handler must fail
                // only its own request with a typed `internal_error`
                // envelope, never silently drop the connection.
                let response = match std::panic::AssertUnwindSafe(handle_request(
                    id.clone(),
                    &method,
                    raw_params,
                    &deps,
                    &token,
                    &health_limiter,
                ))
                .catch_unwind()
                .await
                {
                    Ok(bytes) => bytes,
                    Err(_panic) => jsonrpc_err(
                        id,
                        LoomErrorCode::InternalError,
                        "internal error: request handler panicked",
                    ),
                };
                // Remove from the registry BEFORE enqueueing the response
                // so a serial client reusing the id for its next call can
                // never race the stale entry.
                if !key.is_empty() {
                    cancels.lock().remove(&key);
                }
                let _ = resp_tx.send(response).await;
            });
        }

        // Teardown: fire every still-registered token so in-flight
        // dispatches resolve promptly instead of running out their full
        // server-side deadline against a dead connection. The writer
        // task drains and exits once the last response sender drops.
        for (_, tok) in cancels.lock().drain() {
            tok.cancel();
        }
        dispatch_slots.close();
        drop(resp_tx);
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
    // Strict byte decode: every element must be an integer in 0..=255.
    // The old `as u8` cast wrapped values > 255 (256 → 0, 300 → 44) and
    // `filter_map` silently dropped negative/float entries — a malformed
    // payload could decode to *valid but different* JSON than the client
    // sent and dispatch as a real action. Reject via the same
    // malformed-envelope path as the UTF-8/JSON branches below.
    let bytes: Option<Vec<u8>> = payload_bytes
        .iter()
        .map(|v| v.as_u64().and_then(|n| u8::try_from(n).ok()))
        .collect();
    let bytes = match bytes {
        Some(b) => b,
        None => {
            tracing::warn!(
                "SDK envelope: action.payload contains a non-byte element \
                 (expected integer in 0..=255) — passing params through \
                 unchanged; validator will reject"
            );
            return params;
        }
    };
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

/// Effective per-request deadline: the daemon-wide cap
/// (`LOOM_REQUEST_TIMEOUT_MS`) clamped DOWN by the action's own
/// `deadline_ms` when present. A client can tighten the bound for one
/// action, never extend it past server policy. `0`/absent means "use
/// the server default" (the SDKs send 0 for "no preference").
fn effective_request_timeout(deadline_ms: Option<u64>, cap: Duration) -> Duration {
    match deadline_ms {
        Some(ms) if ms > 0 => Duration::from_millis(ms).min(cap),
        _ => cap,
    }
}

/// Dispatch one parsed request to the router, racing the per-request
/// cancellation token and the server-side deadline. Runs on a spawned
/// per-request task (`request.cancel` and frame-shape errors are handled
/// inline on the read loop before this is reached). The caller owns the
/// cancel-registry entry for `token` and removes it when this returns.
async fn handle_request(
    id: serde_json::Value,
    method: &str,
    raw_params: serde_json::Value,
    deps: &ConnectionHandlerDeps,
    token: &CancellationToken,
    health_limiter: &ConnectionHealthLimiter,
) -> Vec<u8> {
    // Honor the SDK action envelope's per-action `deadline_ms` BEFORE
    // `unwrap_sdk_envelope` discards the envelope keys (it merges only
    // the payload fields + session; `kind`/`deadline_ms` are dropped —
    // pinned by the unwrap tests). Previously this knob was a complete
    // no-op: both SDKs sent it and nothing in the daemon read it.
    let deadline_ms = raw_params
        .get("action")
        .and_then(|a| a.get("deadline_ms"))
        .and_then(|d| d.as_u64());
    let timeout = effective_request_timeout(deadline_ms, request_timeout());

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

    let raw = tokio::select! {
        // Cooperative cancel — `request.cancel` won the race.
        _ = token.cancelled() => {
            tracing::info!(
                metric = "loom_daemon_request_cancelled",
                method = canonical_method,
                request_id = %id_key(&id),
                reason = "client",
            );
            return jsonrpc_err(
                id,
                LoomErrorCode::RequestCancelled,
                "request cancelled by client",
            );
        }
        // Server-side deadline (clamped down by the action's own
        // deadline_ms when one was sent).
        _ = tokio::time::sleep(timeout) => {
            tracing::warn!(
                metric = "loom_daemon_request_timeout",
                method = canonical_method,
                request_id = %id_key(&id),
                timeout_ms = timeout.as_millis() as u64,
                deadline_ms = deadline_ms.unwrap_or(0),
            );
            let message = if timeout < request_timeout() {
                "request exceeded the action's deadline_ms"
            } else {
                "request exceeded server-side per-call deadline"
            };
            return jsonrpc_err(id, LoomErrorCode::RequestTimeout, message);
        }
        // Normal completion.
        bytes = deps.router.dispatch(canonical_method, params) => bytes,
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
