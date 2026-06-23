//! TDD red gate for the `cdp-trusted-input` feature (Phase 2 Step 11).
//!
//! These assert the new public verb surface exists in the action registry:
//!   - `web.press_key` is a registered verb.
//!   - `web.type` carries a `mode` param (fill | value | keystrokes).
//!
//! They FAIL before the feature is built (red) and PASS after Phase 3 adds the
//! registry entries. Behavioral tests (CDP-envelope shapes, host-side routing,
//! replay-equality) are added alongside the implementation in Phase 3.

use loom_rpc::action_registry::ACTIONS;

#[test]
fn web_press_key_is_registered() {
    assert!(
        ACTIONS.iter().any(|a| a.name == "web.press_key"),
        "web.press_key verb should be registered in ACTIONS"
    );
}

#[test]
fn web_type_has_mode_param() {
    let type_verb = ACTIONS
        .iter()
        .find(|a| a.name == "web.type")
        .expect("web.type should be registered");
    let mode = type_verb
        .params
        .iter()
        .find(|p| p.name == "mode")
        .expect("web.type should expose a `mode` param (fill | value | keystrokes)");
    // Lock in the post-flip default: bare `web.type` is `fill` (Playwright fill()
    // semantics — genuine isTrusted insertText that drives react-hook-form).
    assert!(
        mode.doc.contains("\"fill\" (default"),
        "web.type `mode` default must be documented as fill; got: {}",
        mode.doc
    );
}
