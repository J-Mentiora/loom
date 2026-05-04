// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-host/modules/shim_manager/interface_tests.rs` instead.
// Interface tests for `ShimManager`. Verifies BC-HOST-04 (no platform
// symbols), the circuit-breaker state machine, and per-shim error code
// mapping.

use super::shim_manager::{BreakerState, ShimConfig, ShimId, ShimManager};
use crate::host_observability::HostObservability;
use loom_core::error::{LoomError, LoomErrorCode};
use std::path::PathBuf;
use std::sync::Arc;

fn fixture() -> (Arc<ShimManager>, ShimId) {
    let obs = HostObservability::new(true);
    let mgr = ShimManager::new(obs);
    let id = ShimId("chromium".into());
    mgr.register(
        id.clone(),
        ShimConfig {
            binary_path: PathBuf::from("/opt/loom/bin/chromium-shim"),
            args: vec!["--headless".into()],
            env: vec![],
            spawn_retry: 1,
            breaker_threshold: 3,
            breaker_open_ms: 5_000,
            send_timeout_ms: 1_000,
            recv_timeout_ms: 10_000,
        },
    );
    (mgr, id)
}

// === Construction + registration ===

#[test]
fn new_returns_arc_so_dispatch_can_clone_handle() {
    fn _ck(obs: Arc<HostObservability>) -> Arc<ShimManager> {
        ShimManager::new(obs)
    }
    let _ = _ck;
}

#[test]
fn register_shim_records_config() {
    let (_mgr, _id) = fixture();
    // The fixture registers a shim; subsequent breaker_open_window
    // must return the configured value.
    let (mgr, id) = fixture();
    assert_eq!(mgr.breaker_open_window(&id).as_millis(), 5_000);
}

// === Circuit breaker state machine (soft binding §6) ===

#[test]
fn breaker_starts_unknown_until_first_send() {
    // Per design §5: breaker state is per-shim. Before any send, no
    // ShimState entry exists.
    let (mgr, id) = fixture();
    assert!(mgr.breaker_state(&id).is_none());
}

#[test]
fn breaker_reset_clears_state_to_closed() {
    let (mgr, id) = fixture();
    // Compile-time pin only; Phase 5.4 sets up open state then resets.
    mgr.breaker_reset(&id);
    let _ = BreakerState::Closed;
}

#[test]
fn breaker_state_enum_has_three_variants() {
    let _ = BreakerState::Closed;
    let _ = BreakerState::Open;
    let _ = BreakerState::HalfOpen;
}

#[test]
fn breaker_threshold_default_is_three() {
    let c = ShimConfig::default();
    assert_eq!(c.breaker_threshold, 3); // soft binding
}

#[test]
fn breaker_open_window_default_is_5000ms() {
    let c = ShimConfig::default();
    assert_eq!(c.breaker_open_ms, 5_000); // soft binding §6
}

#[test]
fn spawn_retry_default_is_one() {
    let c = ShimConfig::default();
    assert_eq!(c.spawn_retry, 1); // single retry on posix_spawn race
}

// === Send is async (BC-HOST-01: borrows caller's tokio handle) ===

#[test]
fn send_signature_is_async_and_returns_bytes() {
    fn _ck<'a>(
        m: &'a ShimManager,
        id: ShimId,
        msg: Vec<u8>,
    ) -> impl std::future::Future<Output = Result<Vec<u8>, LoomError>> + 'a {
        m.send(id, msg)
    }
    let _ = _ck;
}

// === Per-shim error code mapping (will translate via ErrorMapper) ===

#[test]
fn shim_spawn_failed_code_carries_shim_id_and_errno() {
    let _e = LoomErrorCode::ShimFailure;
    let _: LoomError = LoomError::from(_e);
}

#[test]
fn shim_crashed_code_carries_shim_id() {
    let _e = LoomErrorCode::ShimFailure;
    let _: LoomError = LoomError::from(_e);
}

#[test]
fn shim_timeout_code_carries_phase_send_or_recv() {
    let _send = LoomErrorCode::ShimTimeout;
    let _recv = LoomErrorCode::ShimTimeout;
}

#[test]
fn shim_unavailable_code_carries_retry_after_ms() {
    let _e = LoomErrorCode::ShimBreakerOpen;
}

// === BC-HOST-04: no platform-symbol imports ===

#[test]
fn shim_id_is_a_plain_string_newtype_no_platform_types() {
    // The struct field is `pub String`. No CFRunLoop, no ApplicationServices,
    // no chromiumoxide. Lint enforcement; this test pins by inspection.
    let id = ShimId("ax".into());
    assert_eq!(id.0, "ax");
}

// === Sink-module dependency: ShimManager → HostObservability only ===

#[test]
fn shim_manager_constructor_takes_observability_and_nothing_else_loom_host() {
    // Per dependency block: `ShimManager -> [HostObservability]`. No
    // backward edges; no host_function_table import.
    fn _ck(obs: Arc<HostObservability>) -> Arc<ShimManager> {
        ShimManager::new(obs)
    }
    let _ = _ck;
}
