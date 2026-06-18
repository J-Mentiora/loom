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

/// cross-run determinism (Cluster D): the per-navigation virtual-time budget
/// params MUST carry a finite `budget` AND a `maxVirtualTimeTaskStarvationCount`.
/// The starvation cap guarantees `virtualTimeBudgetExpired` always eventually
/// fires (a busy-loop page can't pin virtual time), which the navigate path now
/// AWAITS before DOM capture — so a missing cap would risk a hang.
#[test]
fn virtual_time_budget_params_have_budget_and_starvation_cap() {
    use ciborium::value::Value;
    use loom_shims::determinism_injector::determinism_injector::build_virtual_time_budget_params;

    let params = build_virtual_time_budget_params();
    let Value::Map(entries) = params else {
        panic!("budget params must be a CBOR map");
    };
    let keys: Vec<String> = entries
        .iter()
        .filter_map(|(k, _)| match k {
            Value::Text(s) => Some(s.clone()),
            _ => None,
        })
        .collect();
    assert!(
        keys.iter().any(|k| k == "budget"),
        "budget params must set a finite virtual-time budget: {keys:?}"
    );
    assert!(
        keys.iter()
            .any(|k| k == "maxVirtualTimeTaskStarvationCount"),
        "budget params must set maxVirtualTimeTaskStarvationCount so a busy page \
         cannot starve virtualTimeBudgetExpired (which navigate now awaits): {keys:?}"
    );
    assert!(
        keys.iter().any(|k| k == "policy"),
        "budget params must set the virtual-time policy: {keys:?}"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// animation-capture (specs/2026-06-18-animation-capture): TDD RED→GREEN guards
// for the three capture-failure modes on framer-motion `whileInView` reveal
// pages. Modes A + C are pinned hermetically here (pure helpers / script source);
// Mode B's host transport leg is pinned in loom-host; the behavioral acceptance
// lives in the gated-live `loom-cli/tests/live_animation_render_regression.rs`.
// ───────────────────────────────────────────────────────────────────────────

/// Mode A (no 30s wedge): after `page_navigate` arms the per-navigation
/// virtual-time budget, the renderer is left PAUSED at the budget horizon; an
/// error/early-return exit that skips the drain leaves it paused so the NEXT
/// `Page.captureScreenshot` blocks to the 30s CDP timeout ("Daemon unresponsive
/// after 30s"). The fix issues a final resume on every exit. The resume params
/// MUST set `policy:"advance"` (un-pause) and carry NO `budget` — a budget would
/// re-pause at expiry and re-wedge the next command.
#[test]
fn virtual_time_resume_params_advance_without_budget() {
    use ciborium::value::Value;
    use loom_shims::determinism_injector::determinism_injector::build_virtual_time_resume_params;

    let Value::Map(entries) = build_virtual_time_resume_params() else {
        panic!("resume params must be a CBOR map");
    };
    let policy = entries.iter().find_map(|(k, v)| match (k, v) {
        (Value::Text(k), Value::Text(v)) if k == "policy" => Some(v.clone()),
        _ => None,
    });
    assert_eq!(
        policy.as_deref(),
        Some("advance"),
        "Mode A resume must set policy:advance to un-pause the renderer so the next \
         command is not wedged on a paused virtual-time clock"
    );
    assert!(
        !entries
            .iter()
            .any(|(k, _)| matches!(k, Value::Text(s) if s == "budget")),
        "Mode A resume must NOT carry a budget — a budget re-pauses at expiry, \
         re-introducing the wedge it is meant to clear"
    );
}

/// Mode C (deterministic reveal capture): `whileInView` reveals are
/// intersection-triggered, so on a never-scrolled page they sit at `opacity:0` and
/// `settled` captures a pre-reveal blank frame. Rather than scroll (IntersectionObserver
/// delivery under CDP virtual time proved unreliable), loom installs a deterministic
/// `IntersectionObserver` override at inject so every observed element is reported
/// intersecting at mount → reveals fire like a mount animation, which virtual time
/// fast-forwards to completion before capture. Hermetic: assert the override's shape.
#[test]
fn reveal_io_override_reports_intersecting_on_observe() {
    use loom_shims::determinism_injector::determinism_injector::REVEAL_IO_OVERRIDE_JS;
    let s = REVEAL_IO_OVERRIDE_JS;
    assert!(
        s.contains("window.IntersectionObserver"),
        "Mode C: must replace window.IntersectionObserver:\n{s}"
    );
    assert!(
        s.contains("isIntersecting: true") || s.contains("isIntersecting:true"),
        "Mode C: the override must report observed elements as intersecting so \
         whileInView reveals fire at mount:\n{s}"
    );
    assert!(
        s.contains(".observe"),
        "Mode C: the override must shim observe():\n{s}"
    );
    // Idempotency guard so a re-injected document doesn't double-wrap.
    assert!(
        s.contains("__loomIoOverridden"),
        "Mode C: the override must be idempotent:\n{s}"
    );
}
