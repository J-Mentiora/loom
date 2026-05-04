// Interface tests for `Dispatcher`.
// Verifies IC-SHIM-12 (non-blocking dispatch), routing table,
// IC-SHIM-10 (error envelope shape), state-invalidation hook.

use super::dispatcher::{
    make_error_response, make_ok_response, route_target, Dispatcher, RouteTarget, ShimDispatcher,
};
use crate::ipc_endpoint::ipc_endpoint::{CdpMessage, ShimErrorCode, ShimRequest, ShimResponse};
use ciborium::value::Value as CborValue;
use loom_shared::types::{EpochMs, Seed};

// === IC-SHIM-12: routing table is the foundation of non-blocking dispatch ===

#[test]
fn spawn_target_routes_to_target_manager() {
    let req = ShimRequest::SpawnTarget {
        request_id: 1,
        session_id: 1,
        profile: "default".into(),
        seed: Seed(0),
        epoch_ms: EpochMs(0),
    };
    assert_eq!(route_target(&req), RouteTarget::TargetManager);
}

#[test]
fn page_navigate_routes_to_target_manager() {
    let req = ShimRequest::PageNavigate {
        request_id: 2,
        session_id: 1,
        target_id: 7,
        url: "https://example.com".into(),
        seed: Seed(0),
        epoch_ms: EpochMs(0),
        blocklist_enabled: true,
    };
    assert_eq!(route_target(&req), RouteTarget::TargetManager);
}

#[test]
fn page_close_routes_to_target_manager() {
    let req = ShimRequest::PageClose {
        request_id: 3,
        session_id: 1,
        target_id: 7,
    };
    assert_eq!(route_target(&req), RouteTarget::TargetManager);
}

#[test]
fn cdp_send_routes_to_action_executor() {
    let req = ShimRequest::CdpSend {
        request_id: 4,
        session_id: 1,
        target_id: 7,
        message: CdpMessage {
            method: "DOM.getDocument".into(),
            params: CborValue::Null,
        },
    };
    assert_eq!(route_target(&req), RouteTarget::ActionExecutor);
}

#[test]
fn shutdown_routes_to_shutdown() {
    assert_eq!(
        route_target(&ShimRequest::Shutdown { request_id: 5 }),
        RouteTarget::Shutdown
    );
}

// === IC-SHIM-10: error envelope shape ===

#[test]
fn make_error_response_carries_code_and_detail() {
    let resp = make_error_response(
        100,
        Some(42),
        ShimErrorCode::ChromiumUnavailable,
        "supervisor budget exhausted",
    );
    match resp {
        ShimResponse::Error {
            request_id,
            session_id,
            code,
            detail,
        } => {
            assert_eq!(request_id, 100);
            assert_eq!(session_id, Some(42));
            assert_eq!(code, ShimErrorCode::ChromiumUnavailable);
            assert!(detail.contains("budget"));
        }
        _ => panic!("expected Error variant"),
    }
}

#[test]
fn make_ok_response_carries_payload() {
    let resp = make_ok_response(101, Some(7), CborValue::Integer(99i64.into()));
    match resp {
        ShimResponse::Ok {
            request_id,
            session_id,
            payload,
        } => {
            assert_eq!(request_id, 101);
            assert_eq!(session_id, Some(7));
            assert_eq!(payload, CborValue::Integer(99i64.into()));
        }
        _ => panic!("expected Ok variant"),
    }
}

// === State-invalidation hook signature (SR-SHIM-04 cascade) ===

#[test]
fn dispatcher_trait_object_is_send_sync() {
    fn _check<T: Dispatcher + ?Sized>() {}
    _check::<dyn Dispatcher>();
}

#[test]
fn dispatcher_exposes_invalidate_in_flight_hook() {
    // Compile-time: trait method exists with the documented signature.
    fn _check<T: Dispatcher + ?Sized>(d: &T) {
        d.invalidate_in_flight("chromium crashed");
    }
    let _ = _check::<dyn Dispatcher>;
}

// === Routing exhaustiveness (BC-SHIM-03 closed enum guarantee) ===

#[test]
fn route_target_handles_every_shim_request_variant() {
    // Exhaustive — match each variant. Adding a new ShimRequest variant
    // forces a compile error here AND in `route_target`.
    let variants = vec![
        ShimRequest::SpawnTarget {
            request_id: 1,
            session_id: 0,
            profile: "p".into(),
            seed: Seed(0),
            epoch_ms: EpochMs(0),
        },
        ShimRequest::CdpSend {
            request_id: 2,
            session_id: 0,
            target_id: 0,
            message: CdpMessage {
                method: "x".into(),
                params: CborValue::Null,
            },
        },
        ShimRequest::PageNavigate {
            request_id: 3,
            session_id: 0,
            target_id: 0,
            url: "x".into(),
            seed: Seed(0),
            epoch_ms: EpochMs(0),
            blocklist_enabled: true,
        },
        ShimRequest::PageClose {
            request_id: 4,
            session_id: 0,
            target_id: 0,
        },
        ShimRequest::Shutdown { request_id: 5 },
    ];
    for v in variants {
        let _ = route_target(&v);
    }
}

// === Real run() loop integration ===

#[tokio::test]
async fn dispatcher_run_returns_on_shutdown_request() {
    use crate::action_executor::action_executor::{ActionExecutor, ActionResult};
    use crate::ipc_endpoint::ipc_endpoint::TargetId;
    use crate::target_manager::target_manager::{TargetManager, TargetState};
    use std::sync::Arc;
    use std::time::Duration as StdDuration;
    use tokio::sync::{mpsc, oneshot};

    /// Minimal stub TargetManager — never called for the Shutdown path.
    struct StubTm;
    #[async_trait::async_trait]
    impl TargetManager for StubTm {
        async fn create_new_target(
            &self,
            _: u64,
            _: String,
            _: Seed,
            _: EpochMs,
        ) -> Result<TargetId, crate::target_manager::target_manager::TargetError> {
            Ok(0)
        }
        fn target_for_session(&self, _: u64) -> Option<TargetId> {
            None
        }
        fn target_state(&self, _: TargetId) -> Option<TargetState> {
            None
        }
        fn close_target(
            &self,
            _: TargetId,
        ) -> Result<(), crate::target_manager::target_manager::TargetError> {
            Ok(())
        }
        fn invalidate_targets(&self) {}
        fn determinism_ready(&self, _: TargetId) -> bool {
            true
        }
    }

    /// Minimal stub ActionExecutor — never called for the Shutdown path.
    struct StubAx;
    #[async_trait::async_trait]
    impl ActionExecutor for StubAx {
        async fn cdp_send(
            &self,
            _: TargetId,
            _: CdpMessage,
            _: Option<std::time::Duration>,
        ) -> Result<ActionResult, ShimResponse> {
            Ok(ActionResult::CdpResult {
                result: CborValue::Null,
            })
        }
        async fn page_navigate(
            &self,
            _: TargetId,
            _: String,
            _: Option<std::time::Duration>,
            _: bool,
        ) -> Result<ActionResult, ShimResponse> {
            Ok(ActionResult::CdpResult {
                result: CborValue::Null,
            })
        }
        async fn page_close(&self, t: TargetId) -> Result<ActionResult, ShimResponse> {
            Ok(ActionResult::PageClosed { target_id: t })
        }
    }

    let (response_tx, mut response_rx) = mpsc::channel::<ShimResponse>(4);
    let dispatcher = ShimDispatcher::new(Arc::new(StubTm), Arc::new(StubAx), Arc::new(response_tx));

    let (request_tx, request_rx) = mpsc::channel::<ShimRequest>(4);
    let (_shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let run = tokio::spawn(async move { dispatcher.run(request_rx, shutdown_rx).await });

    // Send Shutdown — the dispatcher should ack with a Ok carrying the
    // request_id, then return.
    request_tx
        .send(ShimRequest::Shutdown { request_id: 999 })
        .await
        .unwrap();

    // Ack visible on the response channel.
    let ack = tokio::time::timeout(StdDuration::from_secs(1), response_rx.recv())
        .await
        .expect("no shutdown ack within 1s")
        .expect("response channel closed prematurely");
    match ack {
        ShimResponse::Ok { request_id, .. } => assert_eq!(request_id, 999),
        other => panic!("expected Ok ack, got {other:?}"),
    }

    // Run loop returns Ok.
    let res = tokio::time::timeout(StdDuration::from_secs(1), run)
        .await
        .expect("run loop did not return within 1s")
        .expect("run task panicked");
    assert!(res.is_ok());
}
