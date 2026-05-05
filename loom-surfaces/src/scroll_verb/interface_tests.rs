// Interface tests for `ScrollVerb`. Verifies integer-deltas (no floats)
// and the hash-only outcome tier.

extern crate alloc;

use super::scroll_verb::{ScrollAction, ScrollVerb};
use alloc::string::ToString;

#[test]
fn scroll_action_uses_signed_integer_deltas() {
    let a = ScrollAction {
        action_id: "sc".to_string(),
        selector: None,
        delta_x: -50,
        delta_y: 240,
        timeout_ticks: 500,
    };
    let _: i64 = a.delta_x;
    let _: i64 = a.delta_y;
    assert_eq!(a.delta_y, 240);
}

#[test]
fn scroll_action_selector_is_optional_for_root_scroll() {
    let a = ScrollAction {
        action_id: "sc".to_string(),
        selector: None,
        delta_x: 0,
        delta_y: 100,
        timeout_ticks: 500,
    };
    assert!(a.selector.is_none());

    let b = ScrollAction {
        action_id: "sc".to_string(),
        selector: Some(".sidebar".to_string()),
        delta_x: 0,
        delta_y: 100,
        timeout_ticks: 500,
    };
    assert!(b.selector.is_some());
}

#[test]
fn scroll_execute_returns_result_receipt_host_error() {
    let _: fn(ScrollAction) -> Result<_, _> = ScrollVerb::execute;
}

// === Execution test (mock host) ===

#[test]
fn scroll_execute_returns_hash_only_receipt() {
    use crate::host_bindings::host_bindings::mock_host;
    mock_host::setup(vec![0u8; 32]);
    // selector=None → hit_test::resolve_viewport_centre uses
    // Page.getLayoutMetrics. Install a 1024×768 viewport so the centre
    // is (512, 384). Box-model entries are unused on this branch but
    // installed to keep `install_hit_test_box`'s simple signature.
    mock_host::install_hit_test_box(0.0, 0.0, 1.0, 1.0, 1024, 768);

    let action = ScrollAction {
        action_id: "act_scroll".to_string(),
        selector: None,
        delta_x: 0,
        delta_y: 300,
        timeout_ticks: 2_000,
    };

    let receipt = ScrollVerb::execute(action).expect("scroll must return Ok");

    let dom_ref = receipt.dom_after_ref.expect("dom_after_ref must be Some");
    assert_eq!(dom_ref.sha256_hex.len(), 64);
    assert!(dom_ref.sha256_hex.chars().all(|c| c.is_ascii_hexdigit()));

    let ss_ref = receipt
        .screenshot_after_ref
        .expect("screenshot_after_ref must be Some");
    assert_eq!(ss_ref.sha256_hex.len(), 64);
    assert!(ss_ref.sha256_hex.chars().all(|c| c.is_ascii_hexdigit()));

    // Regression guard, None branch: with no selector,
    // the wheel is dispatched at the viewport centre via
    // Page.getLayoutMetrics. 1024×768 viewport → centre (512, 384).
    // Pre-fix this dispatched at (0,0); the assertion below catches
    // any regression to that.
    let dispatches = mock_host::mouse_dispatches();
    assert_eq!(
        dispatches.len(),
        1,
        "scroll dispatches exactly one mouseWheel"
    );
    assert_eq!(dispatches[0].event_type, "mouseWheel");
    assert_eq!((dispatches[0].x, dispatches[0].y), (512, 384));
    assert_eq!(dispatches[0].delta_y, Some(300));
    assert_eq!(dispatches[0].delta_x, Some(0));
    assert_eq!(dispatches[0].button, "none");
}
