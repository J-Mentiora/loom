// Re-export of the locked v5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-rpc/modules/connection_handler/interface_tests.rs` instead.
// Interface tests for `ConnectionHandler`. Verifies FSM
// states, single-task hot path signature,
// deps-injection (no fresh runtime constructed inside).

use super::connection_handler::{
    ConnectionHandler, ConnectionHandlerDeps, ConnectionState, HandlerError,
    AUTHENTICATED_IDLE_TIMEOUT,
};
use crate::auth_middleware::auth_middleware::AuthMiddlewareApi;
use crate::request_router::request_router::RequestRouterApi;
use crate::rpc_observability::rpc_observability::RpcObservabilityApi;
use crate::schema_validator::schema_validator::SchemaValidatorApi;
use std::sync::Arc;
use std::time::Duration;

#[test]
fn fsm_state_has_two_variants_per_ic_rpc_05() {
    let _ = ConnectionState::AwaitingHello;
    let _ = ConnectionState::Authenticated;
}

#[test]
fn authenticated_idle_timeout_is_finite_duration() {
    // Soft binding — value may tune; test asserts type only.
    let _: Duration = AUTHENTICATED_IDLE_TIMEOUT;
    assert!(AUTHENTICATED_IDLE_TIMEOUT.as_secs() > 0);
}

#[test]
fn deps_struct_holds_all_four_arc_handles() {
    fn _ck(d: &ConnectionHandlerDeps) {
        let _: &Arc<dyn AuthMiddlewareApi> = &d.auth;
        let _: &Arc<dyn SchemaValidatorApi> = &d.validator;
        let _: &Arc<dyn RequestRouterApi> = &d.router;
        let _: &Arc<dyn RpcObservabilityApi> = &d.observability;
    }
    let _ = _ck;
}

#[test]
fn handler_constructor_takes_arc_deps_starts_in_awaiting_hello() {
    fn _ck(d: Arc<ConnectionHandlerDeps>) -> ConnectionHandler {
        let h = ConnectionHandler::new(d);
        assert_eq!(h.state, ConnectionState::AwaitingHello);
        h
    }
    let _ = _ck;
}

#[test]
fn run_signature_consumes_unix_stream_async() {
    // runs on the caller's task; no fresh runtime spawned.
    fn _ck() {
        async fn _go(h: ConnectionHandler, s: tokio::net::UnixStream) {
            h.run(s).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

#[test]
fn handler_error_distinguishes_idle_framing_auth_disconnect_io() {
    // design.md §4 error rows.
    let _ = HandlerError::IdleTimeout;
    let _ = HandlerError::FramingFailure {
        reason: "too big".into(),
    };
    let _ = HandlerError::AuthFailed;
    let _ = HandlerError::ClientDisconnected;
    let _ = HandlerError::Io {
        reason: "epipe".into(),
    };
}
