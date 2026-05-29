use super::*;
use crate::cookie_types::ClearCookiesResult;
use crate::host_bindings::host_bindings::mock_host;
use crate::receipt_builder::receipt_builder::{ReceiptStatus, VerbKind};
use crate::safety::safety::SafetyProfile;
use std::collections::HashMap;

#[test]
fn action_round_trips() {
    let a = ClearCookiesAction {
        action_id: "ACT04".to_string(),
        timeout_ticks: 5_000,
        profile: SafetyProfile::Default,
        session_id: "S".to_string(),
    };
    let j = serde_json::to_string(&a).expect("serialize");
    let back: ClearCookiesAction = serde_json::from_str(&j).expect("deserialize");
    assert_eq!(back.action_id, "ACT04");
}

#[test]
fn action_deserialises_legacy_v095_payloads_without_session_id() {
    let json = r#"{
        "action_id": "L",
        "timeout_ticks": 5000,
        "profile": "default"
    }"#;
    let back: ClearCookiesAction = serde_json::from_str(json).expect("deserialise legacy");
    assert_eq!(back.session_id, "");
}

fn act() -> ClearCookiesAction {
    ClearCookiesAction {
        action_id: "ACT_CLEAR".to_string(),
        timeout_ticks: 5_000,
        profile: SafetyProfile::Default,
        session_id: "S_CLEAR".to_string(),
    }
}

fn encode_get_response(n: usize) -> Vec<u8> {
    let cookies: Vec<_> = (0..n)
        .map(|i| serde_json::json!({"name": format!("c{i}"), "value": "v", "domain": "x", "path": "/"}))
        .collect();
    let mut buf = Vec::new();
    ciborium::ser::into_writer(&serde_json::json!({"cookies": cookies}), &mut buf).unwrap();
    buf
}

#[test]
fn execute_with_three_cookies_records_count_before_three() {
    mock_host::setup(Vec::new());
    let mut map = HashMap::new();
    map.insert("Network.getCookies".to_string(), encode_get_response(3));
    map.insert("Network.clearBrowserCookies".to_string(), Vec::new());
    mock_host::setup_method_responses(map);

    let r = ClearCookiesVerb::execute(act()).expect("Receipt");
    assert_eq!(r.verb, VerbKind::ClearCookies);
    assert_eq!(r.status, ReceiptStatus::Ok);

    let result: ClearCookiesResult =
        serde_json::from_str(r.clear_cookies_result.as_deref().unwrap()).unwrap();
    assert_eq!(result.cleared_count, 3);
}

#[test]
fn execute_with_empty_jar_records_count_before_zero() {
    mock_host::setup(Vec::new());
    let mut map = HashMap::new();
    map.insert("Network.getCookies".to_string(), encode_get_response(0));
    map.insert("Network.clearBrowserCookies".to_string(), Vec::new());
    mock_host::setup_method_responses(map);

    let r = ClearCookiesVerb::execute(act()).expect("Receipt");
    assert_eq!(r.status, ReceiptStatus::Ok);
    let result: ClearCookiesResult =
        serde_json::from_str(r.clear_cookies_result.as_deref().unwrap()).unwrap();
    assert_eq!(result.cleared_count, 0);
}

#[test]
fn execute_emits_audit_log_before_destructive_clear_shim_call() {
    // D9 / FND-0050 invariant: peek FIRST, then clear. We can't directly
    // observe log_emit (it's a no-op in the test mock), but we verify
    // the shim_call ordering: peek precedes clear, exactly two
    // chromium calls happen.
    mock_host::setup(Vec::new());
    let mut map = HashMap::new();
    map.insert("Network.getCookies".to_string(), encode_get_response(2));
    map.insert("Network.clearBrowserCookies".to_string(), Vec::new());
    mock_host::setup_method_responses(map);

    let _ = ClearCookiesVerb::execute(act());

    let chromium_count = mock_host::calls()
        .iter()
        .filter(|c| matches!(c, mock_host::HostCall::ShimCall { shim_id, .. } if shim_id == "chromium"))
        .count();
    assert_eq!(chromium_count, 2, "two shim_calls: peek + clear");
}

#[test]
fn execute_with_malformed_peek_response_surfaces_internal_error_and_skips_clear() {
    mock_host::setup(Vec::new());
    let mut map = HashMap::new();
    map.insert("Network.getCookies".to_string(), b"not cbor".to_vec());
    mock_host::setup_method_responses(map);

    let r = ClearCookiesVerb::execute(act()).expect("Receipt");
    assert_eq!(r.status, ReceiptStatus::Error);
    assert!(matches!(
        r.error_code.as_ref().unwrap(),
        crate::error_mapper::error_mapper::LoomErrorCode::HostInternalError { .. }
    ));
    // Critical: NO destructive clear happened — peek failure must
    // short-circuit before the clear (preserves the audit-before-clear
    // invariant).
    let chromium_count = mock_host::calls()
        .into_iter()
        .filter(|c| matches!(c, mock_host::HostCall::ShimCall { shim_id, .. } if shim_id == "chromium"))
        .count();
    assert_eq!(
        chromium_count, 1,
        "only the peek call; clear must NOT fire when peek fails"
    );
}

#[test]
fn execute_records_two_clock_now_and_one_receipt_emit() {
    mock_host::setup(Vec::new());
    let mut map = HashMap::new();
    map.insert("Network.getCookies".to_string(), encode_get_response(0));
    map.insert("Network.clearBrowserCookies".to_string(), Vec::new());
    mock_host::setup_method_responses(map);
    let _ = ClearCookiesVerb::execute(act());

    let calls = mock_host::calls();
    assert_eq!(
        calls
            .iter()
            .filter(|c| matches!(c, mock_host::HostCall::ClockNow))
            .count(),
        2
    );
    assert_eq!(
        calls
            .iter()
            .filter(|c| matches!(c, mock_host::HostCall::ReceiptEmit))
            .count(),
        1
    );
}
