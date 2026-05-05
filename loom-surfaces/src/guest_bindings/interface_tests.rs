// Interface tests for `GuestBindings`. Verifies that all 10 web verbs
// are implemented via wit-bindgen (no hand-rolled exports) and that
// the bytecode is mode-agnostic (same bytes for live and replay).

extern crate alloc;

use super::guest_bindings::{WebSurface, WebSurfaceImpl, WEB_SURFACE_VERBS};
use crate::click_verb::click_verb::ClickAction;
use crate::error_mapper::error_mapper::HostError;
use crate::evaluate_verb::evaluate_verb::EvaluateAction;
use crate::hover_verb::hover_verb::HoverAction;
use crate::navigate_verb::navigate_verb::NavigateAction;
use crate::receipt_builder::receipt_builder::Receipt;
use crate::screenshot_verb::screenshot_verb::ScreenshotAction;
use crate::scroll_verb::scroll_verb::ScrollAction;
use crate::select_verb::select_verb::SelectAction;
use crate::snapshot_verb::snapshot_verb::SnapshotAction;
use crate::type_text_verb::type_text_verb::TypeTextAction;
use crate::wait_verb::wait_verb::WaitAction;

// === 10 verbs, names match the WIT contract ===

#[test]
fn web_surface_exposes_exactly_ten_verbs() {
    assert_eq!(WEB_SURFACE_VERBS.len(), 10);
}

#[test]
fn web_surface_verb_names_match_wit_contract() {
    let expected = [
        "navigate",
        "click",
        "type-text",
        "select",
        "hover",
        "scroll",
        "wait",
        "evaluate",
        "screenshot",
        "snapshot",
    ];
    for name in expected {
        assert!(
            WEB_SURFACE_VERBS.contains(&name),
            "missing WIT-declared verb: {}",
            name
        );
    }
}

// === each WIT method has a corresponding impl method ===

#[test]
fn web_surface_impl_provides_all_ten_methods() {
    // Type-level: each method exists with the right signature
    // `func(action) -> result<receipt, host-error>`. If a verb name in
    // WIT diverges from the impl method, this file fails to compile.
    let _: fn(NavigateAction) -> Result<Receipt, HostError> = WebSurfaceImpl::navigate;
    let _: fn(ClickAction) -> Result<Receipt, HostError> = WebSurfaceImpl::click;
    let _: fn(TypeTextAction) -> Result<Receipt, HostError> = WebSurfaceImpl::type_text;
    let _: fn(SelectAction) -> Result<Receipt, HostError> = WebSurfaceImpl::select;
    let _: fn(HoverAction) -> Result<Receipt, HostError> = WebSurfaceImpl::hover;
    let _: fn(ScrollAction) -> Result<Receipt, HostError> = WebSurfaceImpl::scroll;
    let _: fn(WaitAction) -> Result<Receipt, HostError> = WebSurfaceImpl::wait;
    let _: fn(EvaluateAction) -> Result<Receipt, HostError> = WebSurfaceImpl::evaluate;
    let _: fn(ScreenshotAction) -> Result<Receipt, HostError> = WebSurfaceImpl::screenshot;
    let _: fn(SnapshotAction) -> Result<Receipt, HostError> = WebSurfaceImpl::snapshot;
}

// === no mode-awareness — same bytecode for live + replay ===

#[test]
fn web_surface_methods_take_no_mode_argument() {
    // Each WIT method is `func(action) -> result<receipt, host-error>`.
    // No `mode: Mode` parameter; no `host::current_mode()` call (that
    // host-fn does not exist). The CI lint `tools/lint-surface-mode.py`
    // greps for `Mode::Replay` / `mode\s*==` patterns and fails on any
    // match in the surface crate.
    fn assert_single_arg<A>(_f: fn(A) -> Result<Receipt, HostError>) {}
    assert_single_arg(WebSurfaceImpl::navigate);
    assert_single_arg(WebSurfaceImpl::click);
    assert_single_arg(WebSurfaceImpl::evaluate);
}

// === WIT is source of truth — verb names use kebab-case ===

#[test]
fn type_text_verb_uses_kebab_case_in_wit_name_list() {
    // The Rust method name is `type_text`; the WIT name is `type-text`
    // (kebab-case is WIT convention). The WEB_SURFACE_VERBS array
    // tracks the WIT names so SDKs / RPC routers match.
    assert!(WEB_SURFACE_VERBS.contains(&"type-text"));
    assert!(!WEB_SURFACE_VERBS.contains(&"type_text"));
}
