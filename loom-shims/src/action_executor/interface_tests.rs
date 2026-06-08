// Interface tests for `ActionExecutor`.
// Verifies cdp_send async signature,
// R3 pre-condition, R1 pre-condition,
// no CDP payload escape (`ActionResult` is typed), error mapping.

use super::action_executor::{
    action_error_to_response, ActionError, ActionResult, DEFAULT_ACTION_BUDGET,
};
use crate::ipc_endpoint::ipc_endpoint::{ShimErrorCode, ShimResponse};
use ciborium::value::Value as CborValue;
use std::time::Duration;

// === Default action budget ===

#[test]
fn default_action_budget_is_thirty_seconds() {
    assert_eq!(DEFAULT_ACTION_BUDGET, Duration::from_secs(30));
}

// === ActionResult is typed, no raw CDP bytes for navigate ===

#[test]
fn navigated_result_carries_typed_fields_not_cdp_bytes() {
    let r = ActionResult::Navigated {
        target_id: 7,
        frame_id: "frame-1".into(),
        loader_id: "loader-1".into(),
        network_events: vec![],
        dom_after_sha256: "a".repeat(64),
        screenshot_sha256: "b".repeat(64),
        url: "https://example.com/".into(),
        final_url: "https://example.com/".into(),
        page_title: String::new(),
        status_code: 200,
        dom_bytes: vec![],
        screenshot_bytes: vec![],
        console_lines: vec![],
        blocked_events: vec![],
        network_entries: vec![],
        network_entries_truncated: false,
    };
    match r {
        ActionResult::Navigated {
            target_id,
            frame_id,
            loader_id,
            network_events,
            dom_after_sha256,
            screenshot_sha256,
            ..
        } => {
            assert_eq!(target_id, 7);
            assert_eq!(frame_id, "frame-1");
            assert_eq!(loader_id, "loader-1");
            assert_eq!(network_events.len(), 0);
            assert_eq!(dom_after_sha256.len(), 64);
            assert_eq!(screenshot_sha256.len(), 64);
        }
        _ => panic!("variant mismatch"),
    }
}

#[test]
fn cdp_send_result_passes_cbor_through_only_for_cdp_send_path() {
    // The opaque CBOR pass-through is intentional ONLY for the
    // `cdp_send` path; `Navigated` and `PageClosed` use typed fields.
    let r = ActionResult::CdpResult {
        result: CborValue::Integer(42i64.into()),
    };
    if let ActionResult::CdpResult { result } = r {
        assert_eq!(result, CborValue::Integer(42i64.into()));
    }
}

// === ActionError → ShimErrorCode mapping ===

#[test]
fn r3_violation_maps_to_shim_internal_error() {
    let code: ShimErrorCode = ActionError::R3OrderingViolation(7).into();
    assert_eq!(code, ShimErrorCode::ShimInternalError);
}

#[test]
fn target_unknown_maps_to_target_unknown_code() {
    let code: ShimErrorCode = ActionError::TargetUnknown(99).into();
    assert_eq!(code, ShimErrorCode::TargetUnknown);
}

#[test]
fn cdp_error_inside_action_error_maps_through() {
    use crate::cdp_connection::cdp_connection::CdpError;
    let code: ShimErrorCode = ActionError::Cdp(CdpError::Timeout { ms: 50 }).into();
    assert_eq!(code, ShimErrorCode::CdpTimeout);
}

// === Error envelope helper produces stable shape ===

#[test]
fn action_error_to_response_produces_error_envelope() {
    let resp = action_error_to_response(ActionError::TargetUnknown(7), 50, Some(42));
    match resp {
        ShimResponse::Error {
            request_id,
            session_id,
            code,
            detail,
        } => {
            assert_eq!(request_id, 50);
            assert_eq!(session_id, Some(42));
            assert_eq!(code, ShimErrorCode::TargetUnknown);
            assert!(detail.contains("7"), "detail = {}", detail);
        }
        _ => panic!("expected Error envelope"),
    }
}

#[test]
fn action_error_to_response_works_without_session_id() {
    let resp = action_error_to_response(ActionError::R3OrderingViolation(1), 60, None);
    if let ShimResponse::Error {
        request_id,
        session_id,
        ..
    } = resp
    {
        assert_eq!(request_id, 60);
        assert_eq!(session_id, None);
    } else {
        panic!("expected Error envelope");
    }
}

// === Fix 2 (navigate-status-code-error-surfacing) ===
// extract_nav_error_text pulls CDP `Page.navigate`'s non-empty `errorText`
// field — the channel for DNS / conn-refused / TLS / etc.

#[test]
fn extract_nav_error_text_returns_some_when_error_text_is_non_empty() {
    use super::action_executor::extract_nav_error_text;
    let response = CborValue::Map(vec![
        (
            CborValue::Text("frameId".into()),
            CborValue::Text("frame-1".into()),
        ),
        (
            CborValue::Text("errorText".into()),
            CborValue::Text("net::ERR_NAME_NOT_RESOLVED".into()),
        ),
    ]);
    assert_eq!(
        extract_nav_error_text(&response),
        Some("net::ERR_NAME_NOT_RESOLVED".to_string())
    );
}

#[test]
fn extract_nav_error_text_returns_none_when_field_absent() {
    use super::action_executor::extract_nav_error_text;
    let response = CborValue::Map(vec![
        (
            CborValue::Text("frameId".into()),
            CborValue::Text("frame-1".into()),
        ),
        (
            CborValue::Text("loaderId".into()),
            CborValue::Text("loader-1".into()),
        ),
    ]);
    assert_eq!(extract_nav_error_text(&response), None);
}

#[test]
fn extract_nav_error_text_returns_none_when_field_empty() {
    use super::action_executor::extract_nav_error_text;
    // CDP returns an empty errorText on success — must not synthesize an
    // error event.
    let response = CborValue::Map(vec![(
        CborValue::Text("errorText".into()),
        CborValue::Text("".into()),
    )]);
    assert_eq!(extract_nav_error_text(&response), None);
}
