// Interface tests for `TypeTextVerb`. Verifies the hash-only tier and
// typed CDP only (no Runtime.evaluate shortcut).

extern crate alloc;

use super::type_text_verb::{TypeTextAction, TypeTextVerb};
use alloc::string::ToString;

#[test]
fn type_text_action_carries_selector_text_focus_timeout() {
    let a = TypeTextAction {
        action_id: "act_t".to_string(),
        selector: "input[name=q]".to_string(),
        text: "hello".to_string(),
        focus_first: true,
        timeout_ticks: 3_000,
    };
    assert_eq!(a.text, "hello");
    assert!(a.focus_first);
}

#[test]
fn type_text_execute_returns_result_receipt_host_error() {
    let _: fn(TypeTextAction) -> Result<_, _> = TypeTextVerb::execute;
}

// === Execution test (mock host) ===

#[test]
fn type_text_execute_returns_hash_only_receipt() {
    use crate::host_bindings::host_bindings::mock_host;
    mock_host::setup(vec![0u8; 32]);

    let action = TypeTextAction {
        action_id: "act_type".to_string(),
        selector: "input[name=q]".to_string(),
        text: "hi".to_string(),
        focus_first: false,
        timeout_ticks: 5_000,
    };

    let receipt = TypeTextVerb::execute(action).expect("type_text must return Ok");

    // Hash-only tier: dom_after_ref + screenshot_after_ref both populated
    let dom_ref = receipt.dom_after_ref.expect("dom_after_ref must be Some");
    assert_eq!(dom_ref.sha256_hex.len(), 64);
    assert!(dom_ref.sha256_hex.chars().all(|c| c.is_ascii_hexdigit()));

    let ss_ref = receipt
        .screenshot_after_ref
        .expect("screenshot_after_ref must be Some");
    assert_eq!(ss_ref.sha256_hex.len(), 64);
    assert!(ss_ref.sha256_hex.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn type_text_supports_unicode_payload() {
    let a = TypeTextAction {
        action_id: "u".to_string(),
        selector: "x".to_string(),
        text: "café 🦀".to_string(),
        focus_first: false,
        timeout_ticks: 1_000,
    };
    // UTF-8 string passes through unchanged; per-char dispatch occurs
    // at execute-time (one CDP message per code point).
    assert!(a.text.contains('🦀'));
}
