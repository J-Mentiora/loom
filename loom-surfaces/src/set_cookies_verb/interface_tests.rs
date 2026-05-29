use super::*;
use crate::cookie_types::{CookieSource, NetworkCookieParam, SetCookieResult};
use crate::error_mapper::error_mapper::{HostError, LoomErrorCode, VaultRejectionReason};
use crate::host_bindings::host_bindings::mock_host;
use crate::receipt_builder::receipt_builder::{ReceiptStatus, VerbKind};
use crate::safety::safety::SafetyProfile;
use loom_shared::Redacted;

// ===== serde round-trips (existing v0.9.5 coverage; extended for session_id) =====

#[test]
fn action_struct_round_trips_serde() {
    let a = SetCookiesAction {
        action_id: "ACT01".to_string(),
        source: CookieSource::Inline { cookies: vec![] },
        timeout_ticks: 5_000,
        profile: SafetyProfile::Default,
        session_id: String::new(),
    };
    let json = serde_json::to_string(&a).expect("serialize");
    let back: SetCookiesAction = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.action_id, "ACT01");
    assert_eq!(back.timeout_ticks, 5_000);
    assert!(matches!(back.profile, SafetyProfile::Default));
}

#[test]
fn safety_profile_serializes_to_lowercase() {
    assert_eq!(
        serde_json::to_string(&SafetyProfile::Default).unwrap(),
        "\"default\""
    );
    assert_eq!(
        serde_json::to_string(&SafetyProfile::Safe).unwrap(),
        "\"safe\""
    );
}

#[test]
fn action_deserialises_without_session_id_for_legacy_v095_payloads() {
    // Pre-v0.9.6 actions serialised without `session_id`; the
    // `#[serde(default)]` annotation keeps them readable.
    let json = r#"{
        "action_id": "ACT_LEGACY",
        "source": {"source":"inline","cookies":[]},
        "timeout_ticks": 5000,
        "profile": "default"
    }"#;
    let back: SetCookiesAction = serde_json::from_str(json).expect("deserialise legacy shape");
    assert_eq!(back.session_id, "", "legacy actions default to empty session_id");
}

// ===== execute() tests =====

fn cookie(name: &str, value: &str, domain: Option<&str>) -> NetworkCookieParam {
    NetworkCookieParam {
        name: name.to_string(),
        value: Redacted::new(value.to_string()),
        url: None,
        domain: domain.map(|s| s.to_string()),
        path: None,
        secure: None,
        http_only: None,
        same_site: None,
        expires: None,
        priority: None,
        source_scheme: None,
        source_port: None,
        partition_key: None,
    }
}

fn act_inline(cookies: Vec<NetworkCookieParam>) -> SetCookiesAction {
    SetCookiesAction {
        action_id: "ACT_INLINE".to_string(),
        source: CookieSource::Inline { cookies },
        timeout_ticks: 5_000,
        profile: SafetyProfile::Default,
        session_id: "S_INLINE".to_string(),
    }
}

fn act_grant(grant_id: &str, session_id: &str) -> SetCookiesAction {
    SetCookiesAction {
        action_id: "ACT_GRANT".to_string(),
        source: CookieSource::Grant {
            grant_id: grant_id.to_string(),
        },
        timeout_ticks: 5_000,
        profile: SafetyProfile::Default,
        session_id: session_id.to_string(),
    }
}

#[test]
fn execute_inline_empty_dispatches_cdp_and_emits_ok_receipt() {
    mock_host::setup(Vec::new()); // empty shim response is fine for set_cookies
    let receipt = SetCookiesVerb::execute(act_inline(vec![])).expect("execute returns Receipt");

    assert_eq!(receipt.verb, VerbKind::SetCookies);
    assert_eq!(receipt.status, ReceiptStatus::Ok);
    assert!(receipt.error_code.is_none());

    let result_json = receipt
        .set_cookies_result
        .as_deref()
        .expect("set_cookies_result populated on Ok");
    let results: Vec<SetCookieResult> = serde_json::from_str(result_json).unwrap();
    assert!(results.is_empty(), "no cookies → empty result vec");

    // Receipt was emitted via host::receipt_emit (not just returned).
    let emitted = mock_host::emitted_receipt().expect("receipt_emit captured");
    assert_eq!(emitted.verb, VerbKind::SetCookies);
}

#[test]
fn execute_inline_one_cookie_records_name_and_dispatches_cdp() {
    mock_host::setup(Vec::new());
    let receipt = SetCookiesVerb::execute(act_inline(vec![cookie(
        "sid",
        "abc123",
        Some("example.com"),
    )]))
    .expect("execute returns Receipt");

    assert_eq!(receipt.status, ReceiptStatus::Ok);
    let result_json = receipt.set_cookies_result.as_deref().unwrap();
    let results: Vec<SetCookieResult> = serde_json::from_str(result_json).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "sid");
    assert!(results[0].success);
    assert!(results[0].error_code.is_none());

    // Confirm a shim_call to chromium was made (CDP Network.setCookies).
    let chromium_calls: Vec<_> = mock_host::calls()
        .into_iter()
        .filter(|c| matches!(c, mock_host::HostCall::ShimCall { shim_id, .. } if shim_id == "chromium"))
        .collect();
    assert_eq!(
        chromium_calls.len(),
        1,
        "exactly one CDP envelope dispatched"
    );
}

#[test]
fn execute_rejects_empty_name_with_typed_cookie_validation_error() {
    mock_host::setup(Vec::new());
    let receipt = SetCookiesVerb::execute(act_inline(vec![cookie("", "v", None)]))
        .expect("execute returns Receipt (error variant)");

    assert_eq!(receipt.status, ReceiptStatus::Error);
    let code = receipt.error_code.as_ref().expect("error_code populated");
    match code {
        LoomErrorCode::CookieValidationError(
            crate::cookie_types::CookieValidationError::NameEmpty,
        ) => {}
        other => panic!("expected NameEmpty CookieValidationError, got: {other:?}"),
    }
    assert!(
        receipt.set_cookies_result.is_none(),
        "validation failure short-circuits before populating result vec"
    );
}

#[test]
fn execute_rejects_too_many_cookies_with_typed_error() {
    mock_host::setup(Vec::new());
    // 65 cookies → exceeds the 64-cap.
    let many: Vec<_> = (0..65)
        .map(|i| cookie(&format!("c{i}"), "v", None))
        .collect();
    let receipt = SetCookiesVerb::execute(act_inline(many)).expect("Receipt");

    assert_eq!(receipt.status, ReceiptStatus::Error);
    match receipt.error_code.as_ref().unwrap() {
        LoomErrorCode::CookieValidationError(
            crate::cookie_types::CookieValidationError::TooManyCookies(n),
        ) => {
            assert_eq!(*n, 65);
        }
        other => panic!("expected TooManyCookies(65), got: {other:?}"),
    }
}

#[test]
fn execute_validation_failure_does_not_call_cdp() {
    mock_host::setup(Vec::new());
    let _ = SetCookiesVerb::execute(act_inline(vec![cookie("", "v", None)]));

    // Confirm NO shim_call to chromium happened.
    let chromium_calls: Vec<_> = mock_host::calls()
        .into_iter()
        .filter(|c| matches!(c, mock_host::HostCall::ShimCall { shim_id, .. } if shim_id == "chromium"))
        .collect();
    assert!(
        chromium_calls.is_empty(),
        "validation failure must short-circuit before CDP dispatch"
    );
}

#[test]
fn execute_grant_calls_vault_substitute_cookies_with_grant_and_session_ids() {
    mock_host::setup(Vec::new());
    // Mock vault returns a valid keychain blob with one cookie.
    let blob = serde_json::json!({
        "schema_version": 1,
        "cookies": [
            {"name": "sid", "value": "grant-resolved-value", "domain": "example.com"}
        ]
    });
    mock_host::setup_vault_substitute_cookies(Ok(serde_json::to_vec(&blob).unwrap()));

    let receipt =
        SetCookiesVerb::execute(act_grant("GRANT_42", "SESS_99")).expect("Receipt");

    assert_eq!(receipt.status, ReceiptStatus::Ok);

    // The mock recorded the host-fn call with the right grant + session ids.
    let vault_calls: Vec<_> = mock_host::calls()
        .into_iter()
        .filter_map(|c| match c {
            mock_host::HostCall::VaultSubstituteCookies {
                grant_id,
                session_id,
            } => Some((grant_id, session_id)),
            _ => None,
        })
        .collect();
    assert_eq!(vault_calls.len(), 1);
    assert_eq!(vault_calls[0].0, "GRANT_42");
    assert_eq!(vault_calls[0].1, "SESS_99");

    // The resolved cookie's name surfaces on the receipt; the value does NOT.
    let results: Vec<SetCookieResult> = serde_json::from_str(
        receipt.set_cookies_result.as_deref().unwrap(),
    )
    .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "sid");
    // grant-resolved-value never appears in the receipt — only the name is recorded.
    let receipt_json = serde_json::to_string(&results).unwrap();
    assert!(
        !receipt_json.contains("grant-resolved-value"),
        "raw vault-resolved cookie values must not leak to the receipt"
    );
}

#[test]
fn execute_grant_with_session_mismatch_surfaces_vault_rejection() {
    mock_host::setup(Vec::new());
    // Vault rejects: grant exists but the active session doesn't match its
    // stored session_id (D5 / FND-0008).
    mock_host::setup_vault_substitute_cookies(Err(HostError::VaultRejection {
        reason: VaultRejectionReason::Origin,
    }));

    let receipt =
        SetCookiesVerb::execute(act_grant("GRANT_X", "SESS_WRONG")).expect("Receipt");

    assert_eq!(receipt.status, ReceiptStatus::Error);
    assert!(matches!(
        receipt.error_code.as_ref().unwrap(),
        LoomErrorCode::VaultOriginViolation
    ));
}

#[test]
fn execute_grant_with_malformed_blob_surfaces_internal_error() {
    mock_host::setup(Vec::new());
    mock_host::setup_vault_substitute_cookies(Ok(b"not valid json".to_vec()));

    let receipt = SetCookiesVerb::execute(act_grant("G", "S")).expect("Receipt");
    assert_eq!(receipt.status, ReceiptStatus::Error);
    assert!(matches!(
        receipt.error_code.as_ref().unwrap(),
        LoomErrorCode::HostInternalError { .. }
    ));
}

#[test]
fn execute_grant_with_unsupported_schema_version_surfaces_internal_error() {
    mock_host::setup(Vec::new());
    let blob = serde_json::json!({
        "schema_version": 99,  // unknown future version
        "cookies": [{"name": "sid", "value": "v", "domain": "example.com"}]
    });
    mock_host::setup_vault_substitute_cookies(Ok(serde_json::to_vec(&blob).unwrap()));

    let receipt = SetCookiesVerb::execute(act_grant("G", "S")).expect("Receipt");
    assert_eq!(receipt.status, ReceiptStatus::Error);
    let code = receipt.error_code.as_ref().unwrap();
    let reason = match code {
        LoomErrorCode::HostInternalError { reason } => reason,
        other => panic!("expected HostInternalError, got: {other:?}"),
    };
    assert!(
        reason.contains("schema_version"),
        "internal error should mention schema_version, got: {reason}"
    );
}

#[test]
fn execute_records_two_clock_now_reads_and_one_receipt_emit() {
    mock_host::setup(Vec::new());
    let _ = SetCookiesVerb::execute(act_inline(vec![cookie("sid", "v", None)]));

    let calls = mock_host::calls();
    let clock_count = calls
        .iter()
        .filter(|c| matches!(c, mock_host::HostCall::ClockNow))
        .count();
    let receipt_count = calls
        .iter()
        .filter(|c| matches!(c, mock_host::HostCall::ReceiptEmit))
        .count();
    assert_eq!(clock_count, 2, "exactly two clock_now reads (start + end)");
    assert_eq!(receipt_count, 1, "exactly one receipt_emit");
}
