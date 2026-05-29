use super::*;
use crate::cookie_types::NetworkCookie;
use crate::host_bindings::host_bindings::mock_host;
use crate::receipt_builder::receipt_builder::{ReceiptStatus, VerbKind};
use crate::safety::safety::SafetyProfile;
use std::collections::HashMap;

// ===== serde round-trips =====

#[test]
fn action_round_trips_with_urls() {
    let a = GetCookiesAction {
        action_id: "ACT02".to_string(),
        urls: Some(vec!["http://127.0.0.1/".to_string()]),
        timeout_ticks: 5_000,
        profile: SafetyProfile::Safe,
        session_id: "S".to_string(),
    };
    let j = serde_json::to_string(&a).expect("serialize");
    let back: GetCookiesAction = serde_json::from_str(&j).expect("deserialize");
    assert_eq!(back.urls.as_ref().unwrap().len(), 1);
}

#[test]
fn action_round_trips_with_no_urls() {
    let a = GetCookiesAction {
        action_id: "ACT03".to_string(),
        urls: None,
        timeout_ticks: 5_000,
        profile: SafetyProfile::Default,
        session_id: String::new(),
    };
    let j = serde_json::to_string(&a).expect("serialize");
    let back: GetCookiesAction = serde_json::from_str(&j).expect("deserialize");
    assert!(back.urls.is_none());
}

#[test]
fn action_deserialises_legacy_v095_payloads_without_session_id() {
    let json = r#"{
        "action_id": "ACT_LEGACY",
        "urls": null,
        "timeout_ticks": 5000,
        "profile": "default"
    }"#;
    let back: GetCookiesAction = serde_json::from_str(json).expect("deserialise legacy shape");
    assert_eq!(back.session_id, "");
}

// ===== execute() tests =====

fn act(urls: Option<Vec<String>>) -> GetCookiesAction {
    GetCookiesAction {
        action_id: "ACT_GET".to_string(),
        urls,
        timeout_ticks: 5_000,
        profile: SafetyProfile::Default,
        session_id: "S".to_string(),
    }
}

/// Encode a `{cookies: [...]}` CDP response in CBOR (the shape the
/// chromium shim returns for `Network.getCookies`).
fn encode_get_cookies_response(cookies: &[serde_json::Value]) -> Vec<u8> {
    let mut buf = Vec::new();
    ciborium::ser::into_writer(&serde_json::json!({ "cookies": cookies }), &mut buf).unwrap();
    buf
}

#[test]
fn execute_with_empty_response_returns_empty_cookies_result() {
    mock_host::setup(Vec::new());
    mock_host::setup_method_responses(HashMap::from([(
        "Network.getCookies".to_string(),
        encode_get_cookies_response(&[]),
    )]));

    let r = GetCookiesVerb::execute(act(None)).expect("Receipt");
    assert_eq!(r.verb, VerbKind::GetCookies);
    assert_eq!(r.status, ReceiptStatus::Ok);

    let result_json = r.get_cookies_result.as_deref().expect("result present");
    let cookies: Vec<NetworkCookie> = serde_json::from_str(result_json).unwrap();
    assert!(cookies.is_empty());
}

#[test]
fn execute_with_response_carrying_one_cookie_surfaces_it_on_receipt() {
    mock_host::setup(Vec::new());
    mock_host::setup_method_responses(HashMap::from([(
        "Network.getCookies".to_string(),
        encode_get_cookies_response(&[serde_json::json!({
            "name": "sid",
            "value": "abc123",
            "domain": "example.com",
            "path": "/",
            "expires": -1.0,
            "size": 9,
            "httpOnly": false,
            "secure": true,
            "session": true
        })]),
    )]));

    let r = GetCookiesVerb::execute(act(None)).expect("Receipt");
    assert_eq!(r.status, ReceiptStatus::Ok);
    let cookies: Vec<NetworkCookie> =
        serde_json::from_str(r.get_cookies_result.as_deref().unwrap()).unwrap();
    assert_eq!(cookies.len(), 1);
    assert_eq!(cookies[0].name, "sid");
    assert_eq!(cookies[0].domain, "example.com");
    // Per D7: raw values present in the operator-facing receipt.
    assert_eq!(cookies[0].value.expose(), "abc123");
}

#[test]
fn execute_dispatches_exactly_one_cdp_shim_call() {
    mock_host::setup(Vec::new());
    mock_host::setup_method_responses(HashMap::from([(
        "Network.getCookies".to_string(),
        encode_get_cookies_response(&[]),
    )]));

    let _ = GetCookiesVerb::execute(act(None));
    let chromium_calls = mock_host::calls()
        .into_iter()
        .filter(|c| matches!(c, mock_host::HostCall::ShimCall { shim_id, .. } if shim_id == "chromium"))
        .count();
    assert_eq!(chromium_calls, 1);
}

#[test]
fn execute_with_malformed_cbor_response_surfaces_internal_error() {
    mock_host::setup(Vec::new());
    mock_host::setup_method_responses(HashMap::from([(
        "Network.getCookies".to_string(),
        b"not valid cbor".to_vec(),
    )]));

    let r = GetCookiesVerb::execute(act(None)).expect("Receipt");
    assert_eq!(r.status, ReceiptStatus::Error);
    let code = r.error_code.as_ref().unwrap();
    assert!(matches!(
        code,
        crate::error_mapper::error_mapper::LoomErrorCode::HostInternalError { .. }
    ));
}

#[test]
fn execute_carries_two_clock_now_reads_and_one_receipt_emit() {
    mock_host::setup(Vec::new());
    mock_host::setup_method_responses(HashMap::from([(
        "Network.getCookies".to_string(),
        encode_get_cookies_response(&[]),
    )]));
    let _ = GetCookiesVerb::execute(act(None));

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
