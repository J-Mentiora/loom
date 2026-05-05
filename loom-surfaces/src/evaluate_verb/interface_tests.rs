// Interface tests for `EvaluateVerb`. Verifies IC-SURF-07 console-only
// tier (no DOM/screenshot/network) + carries return value.

extern crate alloc;

use super::evaluate_verb::{EvaluateAction, EvaluateVerb};
use crate::safety::safety::SafetyProfile;
use alloc::string::ToString;

#[test]
fn evaluate_action_carries_expression_await_timeout() {
    let a = EvaluateAction {
        action_id: "e".to_string(),
        expression: "1 + 1".to_string(),
        await_promise: false,
        timeout_ticks: 5_000,
        profile: SafetyProfile::Default,
    };
    assert_eq!(a.expression, "1 + 1");
    assert!(!a.await_promise);
}

#[test]
fn evaluate_action_supports_await_promise() {
    let a = EvaluateAction {
        action_id: "ep".to_string(),
        expression: "fetch('/').then(r => r.status)".to_string(),
        await_promise: true,
        timeout_ticks: 10_000,
        profile: SafetyProfile::Default,
    };
    assert!(a.await_promise);
}

#[test]
fn evaluate_execute_returns_result_receipt_host_error() {
    let _: fn(EvaluateAction) -> Result<_, _> = EvaluateVerb::execute;
}

// === Execution test (mock host) ===

#[test]
fn evaluate_execute_returns_console_receipt() {
    use crate::host_bindings::host_bindings::mock_host;
    mock_host::setup(vec![0x42u8, 0x00]); // shim returns 2 bytes as mock result

    let action = EvaluateAction {
        action_id: "act_eval".to_string(),
        expression: "1 + 1".to_string(),
        await_promise: false,
        timeout_ticks: 5_000,
        profile: SafetyProfile::Default,
    };

    let receipt = EvaluateVerb::execute(action).expect("evaluate must return Ok");

    // Console-only tier (IC-SURF-07): no DOM, no screenshot (AC-WEB-02.2)
    assert!(
        receipt.dom_after_ref.is_none(),
        "evaluate must not capture DOM"
    );
    assert!(
        receipt.screenshot_after_ref.is_none(),
        "evaluate must not capture screenshot"
    );
    // evaluate_return_value must be populated
    assert!(
        receipt.evaluate_return_value.is_some(),
        "evaluate_return_value must be Some"
    );
    let rv = receipt.evaluate_return_value.unwrap();
    assert!(!rv.is_empty(), "return value must be non-empty");
}

// === AC-WEB-07.1: safe profile blocks destructive evaluate ===

#[test]
fn safe_profile_blocks_denylist_evaluate() {
    use crate::error_mapper::error_mapper::LoomErrorCode;
    use crate::host_bindings::host_bindings::mock_host;
    use crate::receipt_builder::receipt_builder::ReceiptStatus;

    mock_host::setup(vec![]); // shim should NOT be called

    let action = EvaluateAction {
        action_id: "act_safe_block".to_string(),
        expression: "document.cookie = ''".to_string(),
        await_promise: false,
        timeout_ticks: 5_000,
        profile: SafetyProfile::Safe,
    };

    let receipt = EvaluateVerb::execute(action).expect("execute must return Ok");

    // Must be an error receipt (AC-WEB-07.1: script NOT executed)
    assert_eq!(
        receipt.status,
        ReceiptStatus::Error,
        "safe profile block must produce error receipt"
    );
    assert_eq!(
        receipt.error_code,
        Some(LoomErrorCode::SchemaViolation),
        "error_code must be SchemaViolation"
    );
    assert_eq!(
        receipt.error_details,
        Some("safe_profile_evaluate_denylist_match".to_string()),
        "error_details must carry violation name"
    );
    // Script must NOT have been executed — no shim call recorded
    let calls = mock_host::calls();
    let shim_calls: Vec<_> = calls
        .iter()
        .filter(|c| {
            matches!(
                c,
                crate::host_bindings::host_bindings::mock_host::HostCall::ShimCall { .. }
            )
        })
        .collect();
    assert!(
        shim_calls.is_empty(),
        "shim must NOT be called when safe profile blocks evaluate"
    );
}

#[test]
fn default_profile_allows_denylist_expression() {
    use crate::host_bindings::host_bindings::mock_host;
    use crate::receipt_builder::receipt_builder::ReceiptStatus;

    mock_host::setup(vec![0xAAu8]);

    let action = EvaluateAction {
        action_id: "act_default_allow".to_string(),
        expression: "document.cookie = ''".to_string(),
        await_promise: false,
        timeout_ticks: 5_000,
        profile: SafetyProfile::Default,
    };

    let receipt = EvaluateVerb::execute(action).expect("execute must return Ok");

    // Default profile: denylist does NOT apply — shim is called
    assert_eq!(
        receipt.status,
        ReceiptStatus::Ok,
        "default profile must allow the expression"
    );
    let calls = mock_host::calls();
    let shim_calls: Vec<_> = calls
        .iter()
        .filter(|c| {
            matches!(
                c,
                crate::host_bindings::host_bindings::mock_host::HostCall::ShimCall { .. }
            )
        })
        .collect();
    assert!(
        !shim_calls.is_empty(),
        "shim must be called for default profile"
    );
}
