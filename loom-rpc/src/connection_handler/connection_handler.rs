// ConnectionHandler — per-connection tokio task running the
// `AwaitingHello → Authenticated` FSM.
//
// # Contract semantics
// - **Per-connection task .** Each accepted connection
//   runs on its own tokio task spawned onto the daemon's shared
//   runtime — no fresh runtime, no global thread pool.
// - **Two-state FSM .**
//     * `AwaitingHello` — read first frame within `HELLO_IDLE_TIMEOUT`;
//       parse + validate via `AuthMiddleware`; close on failure.
//     * `Authenticated` — loop: read frame → validate schema →
//       dispatch via `RequestRouter` → marshal response → write frame.
// - **Single-task hot path .** decode → validate →
//   dispatch → marshal → encode all on the same task. No
//   `tokio::spawn` inside the request loop. Action work runs on
//   loom-host's session task (out-of-band per the contract).
// - **Per-task panic hook (design.md §4).** Handler panics are
//   caught via `JoinHandle::await`'s `JoinError`; `ErrorTranslator`
//   converts the payload to an `InternalError` envelope; the
//   connection is closed; daemon stays up.

use crate::auth_middleware::auth_middleware::AuthMiddlewareApi;
use crate::request_router::request_router::RequestRouterApi;
use crate::rpc_observability::rpc_observability::RpcObservabilityApi;
use crate::schema_validator::schema_validator::SchemaValidatorApi;
use std::sync::Arc;
use std::time::Duration;

/// FSM state per.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    AwaitingHello,
    Authenticated,
}

/// Bundle of `Arc` handles shared across all per-connection tasks.
/// Built once at daemon startup and cloned per `ConnectionHandler`.
pub struct ConnectionHandlerDeps {
    pub auth: Arc<dyn AuthMiddlewareApi>,
    pub validator: Arc<dyn SchemaValidatorApi>,
    pub router: Arc<dyn RequestRouterApi>,
    pub observability: Arc<dyn RpcObservabilityApi>,
}

/// Default idle timeout in `Authenticated` state. Connections idle
/// past this are closed silently to free FDs (soft binding —
/// design.md §8).
pub const AUTHENTICATED_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

#[allow(dead_code)]
pub struct ConnectionHandler {
    pub(crate) deps: Arc<ConnectionHandlerDeps>,
    pub(crate) state: ConnectionState,
}

#[derive(Debug)]
pub enum HandlerError {
    /// Idle timeout reached in either substate.
    IdleTimeout,
    /// Frame > MAX_FRAME_BYTES or codec failure.
    FramingFailure { reason: String },
    /// Auth handshake failed in `AwaitingHello`.
    AuthFailed,
    /// Client closed the connection cleanly.
    ClientDisconnected,
    /// I/O error reading or writing the socket.
    Io { reason: String },
}
