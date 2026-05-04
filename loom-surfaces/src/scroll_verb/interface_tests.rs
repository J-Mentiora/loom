// Interface tests for `ScrollVerb`. Verifies BC-SURF-05 (integer
// deltas, no floats) and IC-SURF-07 hash-only tier.


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

    let ss_ref = receipt.screenshot_after_ref.expect("screenshot_after_ref must be Some");
    assert_eq!(ss_ref.sha256_hex.len(), 64);
    assert!(ss_ref.sha256_hex.chars().all(|c| c.is_ascii_hexdigit()));
}
