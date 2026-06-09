//! Contract test (TDD RED→GREEN) for the faithful-entrance-animations fix.
//!
//! Root cause (specs/2026-06-09-faithful-entrance-animations): the injected
//! determinism script froze `performance.now()` to a constant `0` while a fake
//! `requestAnimationFrame` advanced an independent counter, so JS animation
//! drivers (framer-motion) saw zero elapsed time forever and entrance reveals
//! never progressed past `opacity:0`.
//!
//! This test pins the regression guard at the cheapest hermetic layer (the
//! rendered determinism-script asset — no browser): the script MUST NOT pin
//! `performance.now()` to a constant zero. It is approach-agnostic — it goes
//! GREEN whether the fix removes the JS clock override entirely (CDP virtual
//! time) or replaces the constant with an advancing virtual counter (the
//! documented Option-A fallback).
//!
//! The behavioral acceptance (an opacity:0→1 page captured in its final visible
//! state) lives in the real-Chrome e2e `loom-cli/tests/live_animation_render_regression.rs`.

use loom_shims::determinism_injector::determinism_injector::render_clock_freeze;
use loom_shims::determinism_script_template::render_determinism_script;

const TEMPLATE: &str = include_str!("../assets/determinism_init.js");

/// Normalize away whitespace so the match is not brittle to formatting.
fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

#[test]
fn rendered_script_does_not_freeze_performance_now_to_zero() {
    let rendered = render_determinism_script(TEMPLATE, 0xdead_beef, 1_700_000_000_000);
    let squashed = squash(&rendered);

    // The exact root-cause pattern: `performance.now = function () { return 0; }`.
    assert!(
        !squashed.contains("performance.now=function(){return0;}")
            && !squashed.contains("performance.now=function(){return0}")
            && !squashed.contains("performance.now=()=>0"),
        "REGRESSION (faithful-entrance-animations): the determinism script pins \
         performance.now() to a constant 0. JS animation drivers compute elapsed \
         time from performance.now() deltas, so a frozen clock stalls every \
         entrance/reveal animation at its initial frame. The clock must advance \
         (CDP virtual time) or, in the fallback, performance.now() must be driven \
         by an advancing per-rAF counter — never a constant.\n\nrendered script:\n{rendered}"
    );
    // The main asset must also NOT freeze Date.now / requestAnimationFrame —
    // those are driven by CDP virtual time on the default path.
    assert!(
        !squashed.contains("Date.now=function"),
        "main determinism asset must not override Date.now (virtual time drives it)"
    );
    assert!(
        !squashed.contains("requestAnimationFrame=function"),
        "main determinism asset must not override requestAnimationFrame (native rAF on virtual time)"
    );
}

/// The clock-freeze FALLBACK (virtual time off / CDP failure) must restore the
/// prior DETERMINISTIC frozen clock — so a rollback preserves replay-equality
/// instead of leaking a real-time clock.
#[test]
fn clock_freeze_fallback_freezes_the_clock_deterministically() {
    let frozen = render_clock_freeze(1_700_000_000_000);
    let squashed = squash(&frozen);
    assert!(
        squashed.contains("performance.now=function(){return0;}"),
        "freeze fallback must pin performance.now() to 0 (deterministic): {frozen}"
    );
    assert!(
        squashed.contains("Date.now=function(){return_e;}"),
        "freeze fallback must pin Date.now() to the session epoch: {frozen}"
    );
    assert!(
        frozen.contains("1700000000000") && !frozen.contains("__LOOM_EPOCH_MS__"),
        "freeze fallback must substitute the epoch token: {frozen}"
    );
}
