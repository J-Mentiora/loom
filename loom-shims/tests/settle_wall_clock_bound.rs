//! `wait_for_settle` driver-level regression tests (audit 2026-06-10 +
//! settle-timeout-on-static feature spec) — runnable in the default
//! workspace suite, no fake-chromium binary required.
//!
//! 1. A quiescent "static page" (probe immediately reports
//!    readyState=complete, stable URL, zero mutations, zero in-flight
//!    requests) must yield `SettleOutcome::Reached` — the studio acceptance
//!    criterion that v0.10.1 violated (`settle_outcome:"timeout"` on trivially
//!    static pages). The state-machine pin lives in
//!    `readiness_monitor/interface_tests.rs`; THIS test pins the full driver
//!    loop (`wait_for_settle`), including the probe parse and the
//!    first-tick-url-unstable rule.
//!
//! 2. The wait must be WALL-CLOCK bounded, not just tick-bounded (audit:
//!    "wait_for_settle is tick-bounded but not wall-clock-bounded — navigate
//!    can exceed its budget by orders of magnitude when probes are slow").
//!    With a slow probe and a huge tick ceiling, the outer deadline must end
//!    the wait with a typed `Timeout` instead of grinding through
//!    `tick_ceiling × probe_rtt`.
//!
//! Determinism notes: the verdict assertions are pure functions of the
//! scripted observation sequence; the single wall-clock assertion uses a
//! margin ~25x the expected duration so scheduler jitter cannot flake it.
//! No filesystem, no ports.

use async_trait::async_trait;
use ciborium::value::Value as CborValue;
use loom_shims::cdp_connection::cdp_connection::{
    CdpConnection, CdpError, EventFilter, EventHandler, EventRegistration,
};
use loom_shims::ipc_endpoint::ipc_endpoint::{CdpMessage, TargetId};
use loom_shims::readiness_monitor::{wait_for_settle, SettleConfig, SettleMode, SettleOutcome};
use std::sync::Arc;
use std::time::Duration;

/// Build the CBOR shape `probe_page` parses:
/// `{ result: { value: "<json array text>" } }`.
fn probe_response(ready: bool, href: &str, mutations: u32) -> CborValue {
    let json = format!(r#"[{ready},"{href}",{mutations}]"#);
    CborValue::Map(vec![(
        CborValue::Text("result".into()),
        CborValue::Map(vec![(
            CborValue::Text("value".into()),
            CborValue::Text(json),
        )]),
    )])
}

/// Scripted CDP stub: every `Runtime.evaluate` probe waits `probe_delay`,
/// then reports a fixed page observation. No network events are ever
/// emitted, so the in-flight set stays empty (idle).
struct ScriptedCdp {
    probe_delay: Duration,
    ready: bool,
}

#[async_trait]
impl CdpConnection for ScriptedCdp {
    async fn connect(&self, _ws_url: &str) -> Result<(), CdpError> {
        Ok(())
    }

    async fn command(
        &self,
        _target_id: TargetId,
        _msg: CdpMessage,
        _timeout: Option<Duration>,
    ) -> Result<CborValue, CdpError> {
        if !self.probe_delay.is_zero() {
            tokio::time::sleep(self.probe_delay).await;
        }
        Ok(probe_response(self.ready, "https://static.test/", 0))
    }

    fn register_event_handler(
        &self,
        _filter: EventFilter,
        _handler: EventHandler,
    ) -> EventRegistration {
        EventRegistration::detached(0)
    }

    fn invalidate_session(&self) {}

    fn is_connected(&self) -> bool {
        true
    }
}

/// Studio AC: a fully-loaded, request-quiet, mutation-quiet page MUST yield
/// `settle_outcome: "reached"` well inside the default budget.
#[tokio::test]
async fn static_quiescent_page_reaches_settled_at_driver_level() {
    let cdp: Arc<dyn CdpConnection> = Arc::new(ScriptedCdp {
        probe_delay: Duration::ZERO,
        ready: true,
    });
    let config = SettleConfig {
        idle_threshold: 2,
        quiet_ticks: 3,
        tick_ceiling: 300,
    };
    let result = wait_for_settle(
        &cdp,
        1,
        SettleMode::Settled,
        config,
        true,  // load fired
        false, // already_loaded — no client renavigation in this test
        Duration::from_secs(30),
    )
    .await;

    assert_eq!(
        result.outcome,
        SettleOutcome::Reached,
        "a quiescent static page must settle as Reached, got {:?} after {} ticks",
        result.outcome,
        result.ticks
    );
    assert!(
        result.ticks <= 20,
        "a static page must settle promptly (got {} ticks)",
        result.ticks
    );
    assert_eq!(
        result.network_count, 0,
        "no in-flight requests were scripted"
    );
}

/// Audit pin: slow probes must not multiply the caller's budget. The tick
/// ceiling alone would allow 1_000_000 ticks x 100ms probe RTT here; the
/// outer wall-clock guard must end the wait around `cdp_timeout` with the
/// same typed verdict the tick ceiling uses.
#[tokio::test]
async fn slow_probes_are_wall_clock_bounded_with_typed_timeout() {
    let cdp: Arc<dyn CdpConnection> = Arc::new(ScriptedCdp {
        probe_delay: Duration::from_millis(100),
        ready: false, // never settles
    });
    let config = SettleConfig {
        idle_threshold: 2,
        quiet_ticks: 5,
        tick_ceiling: 1_000_000, // tick bound alone would run for days
    };

    let started = std::time::Instant::now();
    let result = wait_for_settle(
        &cdp,
        1,
        SettleMode::Settled,
        config,
        true,
        false,                      // already_loaded — no client renavigation in this test
        Duration::from_millis(500), // the caller's whole budget
    )
    .await;
    let elapsed = started.elapsed();

    assert_eq!(
        result.outcome,
        SettleOutcome::Timeout,
        "a never-ready page ended by the wall-clock guard must classify as \
         Timeout, got {:?}",
        result.outcome
    );
    // Generous margin (25x the 500ms budget) so CI scheduler jitter cannot
    // flake this; without the wall-clock guard the wait would run for hours.
    assert!(
        elapsed < Duration::from_secs(15),
        "wait_for_settle must be wall-clock bounded near the caller budget; \
         took {elapsed:?} for a 500ms budget"
    );
    assert!(
        result.ticks < 1_000,
        "the wall-clock guard, not the tick ceiling, must have ended the wait \
         (got {} ticks)",
        result.ticks
    );
}

/// client-nav-reattach: an `already_loaded` page (the `wait_for` contract — the
/// caller asserts the document is complete) that the probe reports as `loading`
/// means the page began a top-level navigation it initiated itself. The driver
/// must return early with `renavigated=true` so the caller re-attaches —
/// pinned here at the driver level (no fake-chromium binary needed).
#[tokio::test]
async fn already_loaded_page_observed_loading_reports_renavigated() {
    let cdp: Arc<dyn CdpConnection> = Arc::new(ScriptedCdp {
        probe_delay: Duration::ZERO,
        ready: false, // the new in-flight document is still loading
    });
    let config = SettleConfig {
        idle_threshold: 2,
        quiet_ticks: 3,
        tick_ceiling: 300,
    };
    let result = wait_for_settle(
        &cdp,
        1,
        SettleMode::Settled,
        config,
        true, // load fired
        true, // already_loaded — wait_for asserts the page was complete
        Duration::from_secs(5),
    )
    .await;

    assert!(
        result.renavigated,
        "an already-loaded page observed `loading` must report renavigated so the \
         caller re-attaches (got outcome={:?}, renavigated={})",
        result.outcome, result.renavigated
    );
}

/// client-nav-reattach negative: an `already_loaded` page that stays `complete`
/// (the common `wait_for` case) must settle normally and NEVER spuriously
/// report `renavigated` — the re-arm path is the exception, not the rule (D9).
#[tokio::test]
async fn already_loaded_complete_page_does_not_report_renavigated() {
    let cdp: Arc<dyn CdpConnection> = Arc::new(ScriptedCdp {
        probe_delay: Duration::ZERO,
        ready: true,
    });
    let config = SettleConfig {
        idle_threshold: 2,
        quiet_ticks: 3,
        tick_ceiling: 300,
    };
    let result = wait_for_settle(
        &cdp,
        1,
        SettleMode::Settled,
        config,
        true,
        true, // already_loaded
        Duration::from_secs(5),
    )
    .await;

    assert!(
        !result.renavigated,
        "a complete already-loaded page must NOT report renavigated"
    );
    assert_eq!(
        result.outcome,
        SettleOutcome::Reached,
        "a quiescent already-loaded page must settle as Reached"
    );
}
