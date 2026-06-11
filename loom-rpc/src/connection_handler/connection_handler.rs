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
//     * `Authenticated` — read loop: read frame → spawn a per-request
//       dispatch task (validate schema → dispatch via `RequestRouter` →
//       marshal response) → enqueue response on the writer task.
// - **Concurrent dispatch (connection-protocol redesign).** The read
//   loop never awaits a dispatch: requests run on spawned tasks bounded
//   by a per-connection semaphore (`LOOM_MAX_CONCURRENT_REQUESTS`,
//   default 8), responses are serialized through one writer task and
//   may return OUT OF ORDER — clients correlate by JSON-RPC `id`.
//   `request.cancel` is handled inline on the read loop (fast lane) so
//   it can fire the in-flight token of the request it targets.
// - **Per-task panic isolation (design.md §4).** A panicking handler is
//   caught (`catch_unwind`) on its request task and answered with a
//   typed `internal_error` envelope; the connection stays up.

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
