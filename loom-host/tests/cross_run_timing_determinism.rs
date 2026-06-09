//! Cross-run determinism — harness-clock pinning (Cluster B).
//!
//! TDD red→green for the deterministic per-action timing tick. The session
//! executor must advance the DeterminismHarness virtual clock by a FIXED
//! deterministic delta when determinism is enabled (so receipt `timing_ticks`
//! and the receipt-internal `emitted_at_ms` are reproducible across independent
//! fresh runs), and by the real measured dispatch elapsed only when
//! `--no-determinism` is set.
//!
//! This pins the contract at the pure delta-choice helper so it is unit-testable
//! without standing up a wasmtime Store / shim. The behavioral fake-chromium
//! variant lives in the `#[ignore]` e2e tests; the real-Chromium cross-run
//! `field_diffs=0` proof lives in `tests/e2e/run_e2e.sh` Section 16.

use loom_host::session_executor::{action_delta_ms, DETERMINISTIC_ACTION_TICK_MS};

#[test]
fn deterministic_timing_uses_fixed_tick_regardless_of_wall_clock() {
    // With determinism ON, the delta is the fixed tick for ANY measured
    // dispatch elapsed — that is what makes timing_ticks reproducible run-to-run.
    for elapsed in [0u64, 1, 7, 999, 123_456] {
        assert_eq!(
            action_delta_ms(true, elapsed),
            DETERMINISTIC_ACTION_TICK_MS,
            "deterministic delta must ignore measured elapsed={elapsed}"
        );
    }
}

#[test]
fn deterministic_tick_is_positive_and_reproducible() {
    // Each deterministic action advances the clock by a positive amount (so
    // accumulated timing_ticks stays strictly increasing) and by the SAME amount
    // every run (so two fresh recordings match). Asserted via the fn (not the bare
    // const) so the check is behavioral.
    let first = action_delta_ms(true, 0);
    let second = action_delta_ms(true, 0);
    assert!(first >= 1, "each deterministic tick must be a positive ms");
    assert_eq!(
        first, second,
        "deterministic ticks are identical run-to-run"
    );
}

#[test]
fn non_determinism_preserves_real_elapsed_floored_at_one() {
    // With --no-determinism, keep the existing behavior: real measured elapsed,
    // floored at 1ms (so an action that dispatched in <1ms still advances).
    assert_eq!(action_delta_ms(false, 0), 1, "elapsed 0 floors to 1");
    assert_eq!(action_delta_ms(false, 1), 1);
    assert_eq!(action_delta_ms(false, 42), 42);
    assert_eq!(action_delta_ms(false, 999), 999);
}
