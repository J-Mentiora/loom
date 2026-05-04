// Interface tests for `HoverVerb`. Verifies IC-SURF-07 hash-only tier.


extern crate alloc;

use super::hover_verb::{HoverAction, HoverVerb};
use alloc::string::ToString;

#[test]
fn hover_action_carries_selector_timeout() {
    let a = HoverAction {
        action_id: "h".to_string(),
        selector: "div#tooltip-trigger".to_string(),
        timeout_ticks: 1_000,
    };
    assert_eq!(a.selector, "div#tooltip-trigger");
}

#[test]
fn hover_execute_returns_result_receipt_host_error() {
    let _: fn(HoverAction) -> Result<_, _> = HoverVerb::execute;
}

// === Execution test (mock host) ===

#[test]
fn hover_execute_returns_hash_only_receipt() {
    use crate::host_bindings::host_bindings::mock_host;
    mock_host::setup(vec![0u8; 32]);
    // hit_test sequence: 50×50 box at (200,300)–(250,350); centre (225, 325).
    mock_host::install_hit_test_box(200.0, 300.0, 250.0, 350.0, 1024, 768);

    let action = HoverAction {
        action_id: "act_hover".to_string(),
        selector: "div#tooltip-trigger".to_string(),
        timeout_ticks: 3_000,
    };

    let receipt = HoverVerb::execute(action).expect("hover must return Ok");

    let dom_ref = receipt.dom_after_ref.expect("dom_after_ref must be Some");
    assert_eq!(dom_ref.sha256_hex.len(), 64);
    assert!(dom_ref.sha256_hex.chars().all(|c| c.is_ascii_hexdigit()));

    let ss_ref = receipt.screenshot_after_ref.expect("screenshot_after_ref must be Some");
    assert_eq!(ss_ref.sha256_hex.len(), 64);
    assert!(ss_ref.sha256_hex.chars().all(|c| c.is_ascii_hexdigit()));
}
