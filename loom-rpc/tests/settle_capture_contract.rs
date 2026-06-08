//! TDD RED acceptance contract for the settle-capture feature
//! (spec: specs/2026-06-08-settle-capture/plan.md).
//!
//! These assert the NEW public action-registry contract: the `web.wait_for`
//! readiness verb and the `until` readiness option on `web.navigate` /
//! `web.screenshot`. They FAIL today (the verb/params don't exist yet) and turn
//! GREEN once Phase 3 lands the registry + router + schema changes. The full
//! behavioural suite (fake-chromium e2e for the redirect / animated / async /
//! never-settles shapes, plus the replay byte-equality tests) is enumerated in
//! `specs/2026-06-08-settle-capture/agent-test-spec.md` and built in Phase 3.

use loom_rpc::action_registry::{find, ParamMeta};

fn has_param(verb: &str, param: &str) -> bool {
    find(verb)
        .unwrap_or_else(|| panic!("{verb} must be a registered action"))
        .params
        .iter()
        .any(|p: &ParamMeta| p.name == param)
}

#[test]
fn navigate_gates_capture_on_readiness() {
    // Slice 1. Per D1 the auto-capture is gated on `until` (default settled)
    // instead of firing at navigation commit/load time.
    assert!(
        has_param("web.navigate", "until"),
        "settle-capture: `web.navigate` must expose the `until` readiness option"
    );
    assert!(
        has_param("web.navigate", "timeout_ms"),
        "settle-capture: `web.navigate` must expose `timeout_ms`"
    );
}

#[test]
fn web_wait_for_verb_is_registered() {
    assert!(
        find("web.wait_for").is_some(),
        "settle-capture: `web.wait_for` readiness verb must be registered in the action registry"
    );
    // `until` selects the readiness mode (load|networkidle|settled, default settled);
    // `timeout_ms` is the only caller-tunable bound (quiet-window/threshold are hardcoded).
    assert!(
        has_param("web.wait_for", "until"),
        "web.wait_for must expose `until`"
    );
    assert!(
        has_param("web.wait_for", "timeout_ms"),
        "web.wait_for must expose `timeout_ms`"
    );
}

#[test]
#[ignore = "settle-capture slice 3: web.screenshot until option not built yet"]
fn screenshot_gates_capture_on_readiness() {
    assert!(
        has_param("web.screenshot", "until"),
        "settle-capture: `web.screenshot` must expose the `until` readiness option"
    );
}
