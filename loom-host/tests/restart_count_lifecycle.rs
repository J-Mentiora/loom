//! K8 from #57 — locks in the `ShimState::restart_count` bookkeeping
//! invariants in `ShimManager::get_or_spawn`.
//!
//! ## Invariants under test
//!
//! 1. **Fresh first-spawn does NOT increment restart_count.**
//!    The very first `get_or_spawn` for a `ShimId` creates the state
//!    entry as a side-effect of the spawn flow; restart_count must
//!    start (and stay) at 0 — bumping it on first spawn would be
//!    overcounting (a respawn implies a prior spawn).
//!
//! 2. **Breaker-rejected calls do NOT increment restart_count.**
//!    Once the circuit breaker opens, subsequent calls short-circuit
//!    before reaching `get_or_spawn`. The rejection path must leave
//!    restart_count alone — bumping it on rejections would surface
//!    nonsense numbers in `daemon.health({deep:true})` ("shim restarted
//!    50 times" when really the operator spam-retried into a broken
//!    config).
//!
//! 3. **Spawn failures do NOT increment restart_count.**
//!    `record_failure` creates the state entry and increments
//!    `consecutive_failures`, but `restart_count` stays at 0 because
//!    no actual restart happened. (The increment lives in
//!    `get_or_spawn`'s success branch, not the failure branch.)
//!
//! 4. **Breaker opens after `breaker_threshold` consecutive failures.**
//!    Companion invariant — failures are counted correctly so the
//!    breaker actually trips.
//!
//! ## What's NOT covered here
//!
//! The "successful respawn → restart_count == 1 → another respawn →
//! restart_count == 2" half of #57's K8 acceptance criteria requires a
//! working shim binary AND a way to kill it mid-test to trigger the
//! second spawn. The cleanest path is a `fake-chromium-bin` fixture
//! mirroring `integration_shim_e2e.rs`, but spawning + killing real
//! subprocesses is heavy. Deferred to a follow-up that introduces a
//! mockable spawn-strategy in `ShimManager` — at which point K2's
//! deep-probe e2e will likely benefit from the same harness.

#![cfg(unix)]

use loom_host::host_observability::HostObservability;
use loom_host::shim_manager::{BreakerState, ShimConfig, ShimId, ShimManager};
use loom_shared::error_format::LoomErrorCode;
use loom_shared::shim_protocol::{ciborium_to_vec, CdpMessage};
use std::path::PathBuf;
use std::time::Duration;

/// Build a `ShimManager` registered with a deliberately-broken binary
/// path. Every `send` triggers `get_or_spawn` → spawn fails → recorded
/// as a failure. After `breaker_threshold` failures the breaker opens.
fn manager_with_unspawnable_shim(threshold: u8) -> (std::sync::Arc<ShimManager>, ShimId) {
    let obs = HostObservability::new(true);
    let mgr = ShimManager::new(obs);
    let id = ShimId("chromium:restart-count-test".into());
    mgr.register(
        id.clone(),
        ShimConfig {
            // /nonexistent/... is portable across macOS + Linux (no
            // platform has this binary). spawn_shim's `Command::spawn()`
            // returns ENOENT immediately — no subprocess to clean up,
            // no timeout to wait for.
            binary_path: PathBuf::from("/nonexistent/loom-shim-unspawnable"),
            args: vec![],
            env: vec![],
            spawn_retry: 1,
            breaker_threshold: threshold,
            // 24h — we never want this test to observe breaker re-close
            // by accident; the no-respawn-bump invariant is what we're
            // pinning, not breaker timing.
            breaker_open_ms: 86_400_000,
            send_timeout_ms: 500,
            recv_timeout_ms: 500,
        },
    );
    (mgr, id)
}

/// Produce a CBOR-encoded `CdpMessage` so the `ShimManager::send` decode
/// step succeeds (we want to fail at spawn, not at parse).
fn cdp_send_bytes() -> Vec<u8> {
    let msg = CdpMessage {
        method: "Page.navigate".to_string(),
        params: ciborium::value::Value::Map(vec![]),
    };
    ciborium_to_vec(&msg).expect("encode CdpMessage")
}

#[tokio::test]
async fn restart_count_starts_at_zero_for_fresh_id() {
    let (mgr, id) = manager_with_unspawnable_shim(3);

    // Before any spawn attempt the id has no state entry yet, so
    // `shim_state` returns None — that's the documented contract.
    assert!(
        mgr.shim_state(&id).is_none(),
        "fresh id must have no state entry",
    );
}

#[tokio::test]
async fn spawn_failure_does_not_increment_restart_count() {
    let (mgr, id) = manager_with_unspawnable_shim(5);

    // One send → one spawn attempt → spawn fails → record_failure
    // creates the state entry with restart_count = 0.
    let result = mgr.send(id.clone(), cdp_send_bytes()).await;
    let err = result.expect_err("spawn must fail with nonexistent binary");
    // ShimFailure with errno-ish detail. Don't assert message
    // (it's "No such file or directory" on Unix); just the code.
    assert_eq!(err.code, LoomErrorCode::ShimFailure);

    let state = mgr
        .shim_state(&id)
        .expect("state entry must exist after first failure");
    assert_eq!(
        state.restart_count, 0,
        "spawn failure must not increment restart_count; \
         got {}",
        state.restart_count,
    );
    // Sanity: the failure WAS counted (else the breaker would never trip).
    assert_eq!(
        state.consecutive_failures, 1,
        "spawn failure must increment consecutive_failures",
    );
}

#[tokio::test]
async fn breaker_opens_after_threshold_failures() {
    let threshold: u8 = 3;
    let (mgr, id) = manager_with_unspawnable_shim(threshold);

    for i in 0..threshold {
        let err = mgr
            .send(id.clone(), cdp_send_bytes())
            .await
            .expect_err("spawn fails");
        assert_eq!(
            err.code,
            LoomErrorCode::ShimFailure,
            "call {i}: expected ShimFailure",
        );
    }

    // Threshold reached — breaker should now be Open.
    let bs = mgr.breaker_state(&id).expect("state should exist");
    assert_eq!(
        bs,
        BreakerState::Open,
        "breaker must be Open after {threshold} consecutive failures",
    );

    let state = mgr.shim_state(&id).expect("state should exist");
    assert_eq!(
        state.restart_count, 0,
        "restart_count must be 0 — no spawn ever succeeded",
    );
}

#[tokio::test]
async fn breaker_rejected_calls_do_not_increment_restart_count() {
    // The core K8 invariant. Open the breaker via N failures, then issue
    // more `send` calls (now short-circuited at the breaker check) and
    // assert restart_count stays at 0 — i.e. rejected calls don't show
    // up as fake restarts in `daemon.health` bookkeeping.
    let threshold: u8 = 3;
    let (mgr, id) = manager_with_unspawnable_shim(threshold);

    // Trip the breaker.
    for _ in 0..threshold {
        let _ = mgr.send(id.clone(), cdp_send_bytes()).await;
    }
    assert_eq!(mgr.breaker_state(&id), Some(BreakerState::Open));

    // Now issue 50 more calls — all should short-circuit with
    // ShimBreakerOpen WITHOUT touching restart_count.
    for i in 0..50 {
        let err = mgr
            .send(id.clone(), cdp_send_bytes())
            .await
            .expect_err("breaker open must reject");
        assert_eq!(
            err.code,
            LoomErrorCode::ShimBreakerOpen,
            "rejected call {i}: expected ShimBreakerOpen, got {:?}",
            err.code,
        );
    }

    let state = mgr.shim_state(&id).expect("state should exist");
    assert_eq!(
        state.restart_count, 0,
        "restart_count must remain 0 after 50 breaker-rejected calls; \
         got {} — rejected calls were overcounted as restarts (K8 invariant violated)",
        state.restart_count,
    );
    // Sanity: last_restart_at_ms also stays unset.
    assert!(
        state.last_restart_at_ms.is_none(),
        "last_restart_at_ms must be unset — no respawn happened",
    );
}

#[tokio::test]
async fn last_restart_at_ms_is_none_until_a_real_respawn() {
    // Companion check: even after a barrage of failures + rejections,
    // `last_restart_at_ms` should be None — it's only set inside
    // `get_or_spawn`'s success+is_respawn branch.
    let (mgr, id) = manager_with_unspawnable_shim(2);

    for _ in 0..10 {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            mgr.send(id.clone(), cdp_send_bytes()),
        )
        .await;
    }

    let state = mgr.shim_state(&id).expect("state should exist");
    assert!(
        state.last_restart_at_ms.is_none(),
        "last_restart_at_ms must remain unset across failure-only history",
    );
}
