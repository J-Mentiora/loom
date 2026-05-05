// Interface tests for `SelectVerb`. Verifies the hash-only tier and
// typed-CDP usage.

extern crate alloc;

use super::select_verb::{SelectAction, SelectVerb};
use alloc::string::ToString;

#[test]
fn select_action_carries_selector_value_timeout() {
    let a = SelectAction {
        action_id: "s".to_string(),
        selector: "select#country".to_string(),
        value: "US".to_string(),
        timeout_ticks: 3_000,
    };
    assert_eq!(a.value, "US");
}

#[test]
fn select_execute_returns_result_receipt_host_error() {
    let _: fn(SelectAction) -> Result<_, _> = SelectVerb::execute;
}

// === Execution test (mock host) ===

#[test]
fn select_execute_returns_hash_only_receipt() {
    use crate::host_bindings::host_bindings::mock_host;
    mock_host::setup(vec![0u8; 32]);

    let action = SelectAction {
        action_id: "act_sel".to_string(),
        selector: "select#country".to_string(),
        value: "US".to_string(),
        timeout_ticks: 3_000,
    };

    let receipt = SelectVerb::execute(action).expect("select must return Ok");

    let dom_ref = receipt.dom_after_ref.expect("dom_after_ref must be Some");
    assert_eq!(dom_ref.sha256_hex.len(), 64);
    assert!(dom_ref.sha256_hex.chars().all(|c| c.is_ascii_hexdigit()));

    let ss_ref = receipt
        .screenshot_after_ref
        .expect("screenshot_after_ref must be Some");
    assert_eq!(ss_ref.sha256_hex.len(), 64);
    assert!(ss_ref.sha256_hex.chars().all(|c| c.is_ascii_hexdigit()));
}
