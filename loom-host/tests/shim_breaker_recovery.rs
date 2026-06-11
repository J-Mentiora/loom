//! Locks in circuit-breaker RECOVERY through the public `send` path:
//! the breaker must not stay open forever (the "3 consecutive failures
//! permanently brick the session" bug). After `breaker_open_ms` elapses
//! the next call must be admitted as a HalfOpen probe instead of
//! fail-fasting with `ShimBreakerOpen`; a failed probe re-opens the
//! breaker with a fresh window.
//!
//! Mirrors the `restart_count_lifecycle.rs` harness: an unspawnable
//! binary makes every admitted call fail at spawn (`ShimFailure`), so
//! the error code alone distinguishes "admitted through the breaker"
//! (`ShimFailure`) from "rejected by the breaker" (`ShimBreakerOpen`).

#![cfg(unix)]

use loom_host::host_observability::HostObservability;
use loom_host::shim_manager::{BreakerState, ShimConfig, ShimId, ShimManager};
use loom_shared::error_format::LoomErrorCode;
use loom_shared::shim_protocol::{ciborium_to_vec, CdpMessage};
use std::path::PathBuf;
use std::time::Duration;

/// Short open window so the test waits milliseconds, not the 5 s
/// production default.
const OPEN_WINDOW_MS: u64 = 100;

fn manager_with_unspawnable_shim() -> (std::sync::Arc<ShimManager>, ShimId) {
    let obs = HostObservability::new(true);
    let mgr = ShimManager::new(obs);
    let id = ShimId("chromium:breaker-recovery-test".into());
    mgr.register(
        id.clone(),
        ShimConfig {
            binary_path: PathBuf::from("/nonexistent/loom-shim-unspawnable"),
            args: vec![],
            env: vec![],
            spawn_retry: 1,
            breaker_threshold: 3,
            breaker_open_ms: OPEN_WINDOW_MS,
            send_timeout_ms: 500,
            recv_timeout_ms: 500,
        },
    );
    (mgr, id)
}

fn cdp_send_bytes() -> Vec<u8> {
    let msg = CdpMessage {
        method: "Page.navigate".to_string(),
        params: ciborium::value::Value::Map(vec![]),
    };
    ciborium_to_vec(&msg).expect("encode CdpMessage")
}

#[tokio::test]
async fn breaker_admits_probe_after_open_window_and_reopens_on_probe_failure() {
    let (mgr, id) = manager_with_unspawnable_shim();

    // Trip the breaker: 3 consecutive spawn failures.
    for i in 0..3 {
        let err = mgr
            .send(id.clone(), cdp_send_bytes())
            .await
            .expect_err("spawn must fail");
        assert_eq!(err.code, LoomErrorCode::ShimFailure, "call {i}");
    }
    assert_eq!(mgr.breaker_state(&id), Some(BreakerState::Open));

    // Within the open window: fail-fast, no spawn attempt.
    let err = mgr
        .send(id.clone(), cdp_send_bytes())
        .await
        .expect_err("breaker open must reject");
    assert_eq!(err.code, LoomErrorCode::ShimBreakerOpen);

    // Let the open window expire. The next call must be ADMITTED as a
    // HalfOpen probe — it reaches the spawn path again (ShimFailure),
    // not the breaker rejection (ShimBreakerOpen). Before the fix the
    // breaker had no expiry and this rejected forever.
    tokio::time::sleep(Duration::from_millis(OPEN_WINDOW_MS + 50)).await;
    let err = mgr
        .send(id.clone(), cdp_send_bytes())
        .await
        .expect_err("probe must fail at spawn");
    assert_eq!(
        err.code,
        LoomErrorCode::ShimFailure,
        "after the window the call must be admitted as a probe, not breaker-rejected"
    );

    // The failed probe re-opened the breaker with a FRESH window.
    assert_eq!(mgr.breaker_state(&id), Some(BreakerState::Open));
    let err = mgr
        .send(id.clone(), cdp_send_bytes())
        .await
        .expect_err("re-opened breaker must reject");
    assert_eq!(err.code, LoomErrorCode::ShimBreakerOpen);

    // And the cycle repeats: a second expiry admits another probe.
    tokio::time::sleep(Duration::from_millis(OPEN_WINDOW_MS + 50)).await;
    let err = mgr
        .send(id.clone(), cdp_send_bytes())
        .await
        .expect_err("second probe must fail at spawn");
    assert_eq!(err.code, LoomErrorCode::ShimFailure);
}
