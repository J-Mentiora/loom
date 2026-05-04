//! J.3: target_manager `create_new_target` MUST await the determinism
//! inject and flip the `determinism_injected` flag ONLY on `Ok(())`.
//! These tests are the regression fitness function for the primary bug
//! (`drop(self.determinism.inject(...))` at the old call site).

use ciborium::value::Value;
use loom_shared::types::{EpochMs, Seed};
use loom_shims::cdp_connection::cdp_connection::{
    CdpConnection, CdpError, EventFilter, EventHandler, EventRegistration,
};
use loom_shims::determinism_injector::determinism_injector::{
    ChromiumDeterminismInjector, ADD_SCRIPT_METHOD,
};
use loom_shims::ipc_endpoint::ipc_endpoint::{CdpMessage, ResponseSender, TargetId};
use loom_shims::target_manager::target_manager::{ChromiumTargetManager, TargetManager};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

const TEMPLATE: &str = include_str!("../assets/determinism_init.js");

/// CdpConnection mock that records every command and replies Ok with a
/// canonical `{"identifier": "..."}` response (mirrors the chromium
/// `Page.addScriptToEvaluateOnNewDocument` reply shape).
struct RecordingCdp {
    calls: Mutex<Vec<(TargetId, String)>>,
}

impl RecordingCdp {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
        })
    }
    fn methods(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|(_, m)| m.clone())
            .collect()
    }
}

#[async_trait::async_trait]
impl CdpConnection for RecordingCdp {
    async fn connect(&self, _ws_url: &str) -> Result<(), CdpError> {
        Ok(())
    }
    async fn command(
        &self,
        t: TargetId,
        msg: CdpMessage,
        _: Option<Duration>,
    ) -> Result<Value, CdpError> {
        self.calls.lock().unwrap().push((t, msg.method.clone()));
        Ok(Value::Map(vec![(
            Value::Text("identifier".into()),
            Value::Text("script-id-1".into()),
        )]))
    }
    fn register_event_handler(&self, _: EventFilter, _: EventHandler) -> EventRegistration {
        EventRegistration { handler_id: 0 }
    }
    fn invalidate_session(&self) {}
    fn is_connected(&self) -> bool {
        true
    }
}

/// CdpConnection that always returns CdpError::Timeout — exercises the
/// `Err` path of `inject` so we can prove `determinism_injected` stays
/// false and `by_session` is NOT populated.
struct AlwaysFailingCdp;
#[async_trait::async_trait]
impl CdpConnection for AlwaysFailingCdp {
    async fn connect(&self, _ws_url: &str) -> Result<(), CdpError> {
        Ok(())
    }
    async fn command(
        &self,
        _: TargetId,
        _: CdpMessage,
        _: Option<Duration>,
    ) -> Result<Value, CdpError> {
        Err(CdpError::Timeout { ms: 0 })
    }
    fn register_event_handler(&self, _: EventFilter, _: EventHandler) -> EventRegistration {
        EventRegistration { handler_id: 0 }
    }
    fn invalidate_session(&self) {}
    fn is_connected(&self) -> bool {
        true
    }
}

fn build_response_tx() -> ResponseSender {
    let (tx, _rx) = mpsc::channel(8);
    Arc::new(tx)
}

#[tokio::test]
async fn create_new_target_actually_awaits_inject_and_sends_cdp_command() {
    // Regression test for the primary bug: previous body did
    // `drop(self.determinism.inject(target_id))` — async future was
    // dropped without polling, so `Page.addScriptToEvaluateOnNewDocument`
    // was never sent. RecordingCdp would have observed zero
    // `addScriptToEvaluateOnNewDocument` calls. New body awaits the
    // future, so exactly one call is recorded.
    let cdp = RecordingCdp::new();
    let injector = ChromiumDeterminismInjector::new(cdp.clone(), TEMPLATE.to_string());
    let mgr = ChromiumTargetManager::new(cdp.clone(), injector, build_response_tx());

    let target_id = mgr
        .create_new_target(1, "default".into(), Seed(42), EpochMs(0))
        .await
        .expect("create_new_target must succeed when inject succeeds");

    let methods = cdp.methods();
    assert_eq!(
        methods
            .iter()
            .filter(|m| m.as_str() == ADD_SCRIPT_METHOD)
            .count(),
        1,
        "exactly one addScriptToEvaluateOnNewDocument must be sent on the wire"
    );
    assert!(
        mgr.determinism_ready(target_id),
        "flag must be true after Ok(()) from awaited inject"
    );
}

#[tokio::test]
async fn create_new_target_failed_inject_leaves_flag_false_and_no_session_binding() {
    let cdp_failing = Arc::new(AlwaysFailingCdp);
    let injector = ChromiumDeterminismInjector::new(cdp_failing.clone(), TEMPLATE.to_string());
    let mgr = ChromiumTargetManager::new(cdp_failing.clone(), injector, build_response_tx());

    let result = mgr
        .create_new_target(2, "default".into(), Seed(42), EpochMs(0))
        .await;
    assert!(result.is_err(), "inject failure must propagate");

    // Re-trying must NOT short-circuit through a poisoned by_session map —
    // the second create_new_target also exercises a fresh inject attempt.
    let result2 = mgr
        .create_new_target(2, "default".into(), Seed(42), EpochMs(0))
        .await;
    assert!(
        result2.is_err(),
        "retry must reach inject again, not poisoned cache"
    );

    // No target should be bound to session 2 after failed creates.
    assert!(
        mgr.target_for_session(2).is_none(),
        "by_session must NOT bind a target after failed inject"
    );
}

#[tokio::test]
async fn create_new_target_idempotent_per_session() {
    // SR-SHIM-01: same session_id → same target_id; second call reuses,
    // doesn't re-inject.
    let cdp = RecordingCdp::new();
    let injector = ChromiumDeterminismInjector::new(cdp.clone(), TEMPLATE.to_string());
    let mgr = ChromiumTargetManager::new(cdp.clone(), injector, build_response_tx());

    let t1 = mgr
        .create_new_target(7, "default".into(), Seed(42), EpochMs(0))
        .await
        .unwrap();
    let t2 = mgr
        .create_new_target(7, "default".into(), Seed(42), EpochMs(0))
        .await
        .unwrap();
    assert_eq!(t1, t2, "same session must yield same target_id");

    let methods = cdp.methods();
    assert_eq!(
        methods
            .iter()
            .filter(|m| m.as_str() == ADD_SCRIPT_METHOD)
            .count(),
        1,
        "second create_new_target must NOT re-inject"
    );
}
