//! J.7b — V8 seed-determinism integration test (`#[ignore]`).
//!
//! Marked `#[ignore]` so default `cargo test` skips it. Exercised by:
//!   - `cargo test --ignored` for full validation
//!   - Phase 8 verify-app for AC-RNGDET-04 acceptance
//!
//! This is the closing link in the AC chain:
//!
//!   J.6 sfc32_golden  (Rust↔JS algorithm parity)
//!   J.3 target_manager_seed_threading  (inject is awaited, seed reaches CDP)
//!   J.5 shim_protocol round-trip  (wire carries seed correctly)
//!   J.7a seed_threading_e2e  (Session.seed receives the right value)
//!   J.7b V8 seed_determinism  ← this file: V8 actually evaluates the
//!                                 substituted JS and produces the golden.
//!
//! Implementation is deferred — the `loom-shim-chromium` test harness
//! requires real Chromium installed locally and the chromiumoxide
//! end-to-end wiring is still phase-6 stub. When that lands, replace
//! the `#[ignore]` body below with the real subprocess + CDP roundtrip.

#[ignore]
#[tokio::test]
async fn two_sessions_seed_42_produce_byte_identical_math_random() {
    // Spec:
    //   1. Spawn `loom-shim-chromium` subprocess.
    //   2. Send `ShimRequest::SpawnTarget { ..., seed: Seed(42), epoch_ms: EpochMs(0) }`.
    //   3. Send `ShimRequest::PageNavigate { ..., url: "data:text/html,<html></html>" }`.
    //   4. Send a CDP `Runtime.evaluate { expression: "Math.random().toFixed(8)" }`.
    //   5. Assert the result equals `0.63217048` (the J.6 golden for sfc32(seed=42)).
    //   6. Tear down. Repeat with a fresh subprocess + same seed=42; assert IDENTICAL.
    //   7. Repeat with seed=0; assert DIFFERENT from seed=42.
    //
    // AC-RNGDET-01..04 verified end-to-end against real V8.
    //
    // Until the loom-shim-chromium test harness is wired (Phase 6+), this
    // test panics. Phase 8 verify-app gates on this test passing.
    panic!("V8 seed-determinism integration test deferred to Phase 6+ chromium subprocess wiring");
}

#[ignore]
#[tokio::test]
async fn one_hundred_runs_with_same_seed_produce_zero_divergence() {
    // AC-RNGDET-03: bit-equal replay across 100 runs.
    //
    // Spec: spawn 100 fresh sessions with seed=42, capture each session's
    // first 1000 `Math.random()` outputs as a hex digest, assert all 100
    // digests are identical.
    panic!("V8 100-run determinism test deferred to Phase 6+ chromium subprocess wiring");
}
