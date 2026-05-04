// Interface tests for `WaitVerb`. Verifies IC-SURF-02 (clock_now-driven
// polling, no `std::thread::sleep`), IC-SURF-07 hash-only tier.

extern crate alloc;

use super::wait_verb::{WaitAction, WaitVerb};
use alloc::string::ToString;

#[test]
fn wait_action_carries_predicate_timeout_poll_interval() {
    let a = WaitAction {
        action_id: "w".to_string(),
        predicate_js: "document.querySelector('.ready') !== null".to_string(),
        timeout_ticks: 30_000,
        poll_interval_ticks: 100,
    };
    let _: u64 = a.timeout_ticks;
    let _: u64 = a.poll_interval_ticks;
    assert!(a.predicate_js.contains("ready"));
}

#[test]
fn wait_execute_returns_result_receipt_host_error() {
    let _: fn(WaitAction) -> Result<_, _> = WaitVerb::execute;
}

// === Execution tests (mock host) ===

#[test]
fn wait_execute_returns_receipt_when_predicate_truthy() {
    use crate::host_bindings::host_bindings::mock_host;
    // Non-zero response → truthy predicate on first poll
    mock_host::setup(vec![0x01u8]);

    let action = WaitAction {
        action_id: "act_wait".to_string(),
        predicate_js: "document.querySelector('.ready') !== null".to_string(),
        timeout_ticks: 10_000,
        poll_interval_ticks: 100,
    };

    let receipt = WaitVerb::execute(action).expect("wait must return Ok on truthy predicate");

    // Screenshot-only tier: screenshot present, no DOM blob
    assert!(
        receipt.screenshot_after_ref.is_some(),
        "screenshot_after_ref must be Some"
    );
    assert!(
        receipt.dom_after_ref.is_none(),
        "wait must not capture DOM blob"
    );
}

#[test]
fn wait_execute_returns_error_receipt_on_timeout() {
    use crate::host_bindings::host_bindings::mock_host;
    use crate::receipt_builder::receipt_builder::ReceiptStatus;
    // All-zero response → falsy; will exhaust timeout
    mock_host::setup(vec![0u8; 1]);

    let action = WaitAction {
        action_id: "act_wait_to".to_string(),
        predicate_js: "false".to_string(),
        timeout_ticks: 3_000,
        poll_interval_ticks: 100,
    };

    let receipt = WaitVerb::execute(action).expect("wait must return Ok (error receipt)");
    assert_eq!(
        receipt.status,
        ReceiptStatus::Error,
        "timed-out wait must produce error receipt"
    );
}
