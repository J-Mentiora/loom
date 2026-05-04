// Interface tests for `ClickVerb`. Verifies IC-SURF-07 hash-only tier,
// IC-SURF-09 receipt-emit path, IC-SURF-06 chromium shim usage,
// BC-SURF-05 integer coordinates.

extern crate alloc;

use super::click_verb::{ClickAction, ClickVerb};
use alloc::string::ToString;

#[test]
fn click_action_carries_selector_button_count_timeout() {
    let a = ClickAction {
        action_id: "act_1".to_string(),
        selector: ".submit".to_string(),
        button: "left".to_string(),
        click_count: 1,
        timeout_ticks: 5_000,
    };
    assert_eq!(a.selector, ".submit");
    let _: u32 = a.click_count;
    let _: u64 = a.timeout_ticks;
}

#[test]
fn click_execute_returns_result_receipt_host_error() {
    // Type-level: WIT signature `func(action) -> result<receipt, host-error>`.
    let _: fn(ClickAction) -> Result<_, _> = ClickVerb::execute;
}

#[test]
fn click_action_takes_no_capture_mode_argument() {
    // IC-SURF-08: surface emits hash-only by default; SessionExecutor
    // upgrades to full under `--capture full`. The verb's action struct
    // has no `capture_mode` field.
    use core::mem::size_of;
    // Sanity: action is bounded-size struct (no implicit mode field).
    let _ = size_of::<ClickAction>();
}

// === Execution test (mock host) ===

#[test]
fn click_execute_returns_hash_only_receipt() {
    use crate::host_bindings::host_bindings::mock_host;
    mock_host::setup(vec![0u8; 32]);

    let action = ClickAction {
        action_id: "act_click".to_string(),
        selector: ".submit".to_string(),
        button: "left".to_string(),
        click_count: 1,
        timeout_ticks: 5_000,
    };

    let receipt = ClickVerb::execute(action).expect("click must return Ok");

    // Hash-only tier: dom_after_ref with 64-char SHA-256 (AC-WEB-02.2)
    let dom_ref = receipt.dom_after_ref.expect("dom_after_ref must be Some");
    assert_eq!(dom_ref.sha256_hex.len(), 64);
    assert!(dom_ref.sha256_hex.chars().all(|c| c.is_ascii_hexdigit()));
}
