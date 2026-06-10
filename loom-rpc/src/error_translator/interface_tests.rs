// Interface tests for `ErrorTranslator`. Verifies the
// 1:1 envelope shape, 280-char message cap, panic-to-envelope path.

use super::error_translator::{
    ErrorTranslator, JsonRpcError, LoomErrorCode, SchemaViolationDetail, MAX_MESSAGE_LEN,
};
use std::any::Any;

#[test]
fn envelope_has_code_message_data_fields() {
    fn _ck(e: &JsonRpcError) {
        let _: &LoomErrorCode = &e.code;
        let _: &String = &e.message;
        let _: &Option<serde_json::Value> = &e.data;
    }
    let _ = _ck;
}

#[test]
fn loom_error_code_serialises_as_snake_case_string() {
    let s = serde_json::to_string(&LoomErrorCode::ProtocolAuthRequired).unwrap();
    assert_eq!(s, "\"protocol_auth_required\"");
    let s = serde_json::to_string(&LoomErrorCode::SchemaViolation).unwrap();
    assert_eq!(s, "\"schema_violation\"");
    let s = serde_json::to_string(&LoomErrorCode::SessionAborted).unwrap();
    assert_eq!(s, "\"session_aborted\"");
}

#[test]
fn loom_error_code_covers_protocol_layer_variants() {
    // Protocol-layer variant coverage.
    let _ = LoomErrorCode::ProtocolAuthRequired;
    let _ = LoomErrorCode::ProtocolMalformed;
    let _ = LoomErrorCode::SchemaViolation;
    let _ = LoomErrorCode::MethodNotFound;
}

#[test]
fn loom_error_code_mirrors_core_and_host_variants() {
    // 1:1 enum mirroring loom-core / loom-host variants.
    let _ = LoomErrorCode::SessionNotFound;
    let _ = LoomErrorCode::SessionAborted;
    let _ = LoomErrorCode::BudgetExceeded;
    let _ = LoomErrorCode::SurfaceTrap;
    let _ = LoomErrorCode::SurfaceUnavailable;
    let _ = LoomErrorCode::VaultGrantNotFound;
    let _ = LoomErrorCode::VaultGrantRevoked;
    let _ = LoomErrorCode::VaultCredentialTypeUnsupported;
    let _ = LoomErrorCode::StoreIntegrityFailed;
    let _ = LoomErrorCode::InternalError;
}

#[test]
fn schema_violation_carries_field_expected_actual() {
    fn _ck(d: SchemaViolationDetail) -> JsonRpcError {
        ErrorTranslator::from_schema_violation(d)
    }
    let _ = _ck;
    let d = SchemaViolationDetail {
        field: "params.session_id".into(),
        expected: "string".into(),
        actual: "null".into(),
    };
    let _: String = d.field;
}

#[test]
fn message_cap_constant_is_280() {
    assert_eq!(MAX_MESSAGE_LEN, 280);
}

#[test]
fn truncate_message_ascii_over_cap_keeps_byte_budget_and_ellipsis() {
    let msg = "x".repeat(MAX_MESSAGE_LEN + 100);
    let out = ErrorTranslator::truncate_message(&msg);
    assert_eq!(out.len(), MAX_MESSAGE_LEN, "277 bytes + '...' == 280");
    assert!(out.ends_with("..."));
}

#[test]
fn truncate_message_at_or_under_cap_is_unchanged() {
    let msg = "é".repeat(MAX_MESSAGE_LEN / 2); // exactly 280 bytes
    assert_eq!(ErrorTranslator::truncate_message(&msg), msg);
}

#[test]
fn truncate_message_multibyte_straddling_cut_point_does_not_panic() {
    // Regression: the cut point used to be a fixed byte slice, which
    // panics when byte 277 falls inside a multi-byte char — a
    // whole-daemon abort under panic = "abort". 'é' is 2 bytes, so
    // byte 277 is mid-char; '世' is 3 bytes, ditto.
    for msg in [
        "é".repeat(200),        // 400 bytes
        "\u{4e16}".repeat(100), // 300 bytes of CJK
        format!("unknown profile: {}", "界".repeat(100)),
    ] {
        let out = ErrorTranslator::truncate_message(&msg);
        assert!(out.len() <= MAX_MESSAGE_LEN, "must respect the cap: {out}");
        assert!(out.ends_with("..."), "must keep the ellipsis: {out}");
    }
}

#[test]
fn from_unknown_profile_with_long_multibyte_profile_does_not_panic() {
    // End-to-end: the profile string is client-controlled and embeds
    // straight into the message that truncate_message cuts.
    let provided = "界".repeat(120);
    let env = ErrorTranslator::from_unknown_profile(&provided, &["safe", "standard", "full"]);
    assert!(env.message.len() <= MAX_MESSAGE_LEN);
    assert_eq!(env.code, LoomErrorCode::UnknownProfile);
}

#[test]
fn panic_payload_converts_to_internal_error_envelope() {
    fn _ck(p: Box<dyn Any + Send>) -> JsonRpcError {
        ErrorTranslator::catch_panic_into_envelope(p)
    }
    let _ = _ck;
}

// ===== Typed validation envelopes =====

#[test]
fn from_unknown_profile_serialises_correctly() {
    let env = ErrorTranslator::from_unknown_profile("nonexistent", &["safe", "standard", "full"]);
    let s = serde_json::to_string(&env).unwrap();
    assert!(
        s.contains("\"code\":\"unknown_profile\""),
        "envelope must serialise code=unknown_profile, got {s}"
    );
    assert!(s.contains("\"provided\":\"nonexistent\""), "envelope: {s}");
    assert!(s.contains("\"available\""), "envelope: {s}");
    assert!(env.message.len() <= MAX_MESSAGE_LEN);
    assert_eq!(env.code, LoomErrorCode::UnknownProfile);
}

#[test]
fn from_invalid_network_mode_serialises_correctly() {
    let env = ErrorTranslator::from_invalid_network_mode("bogus", &["live", "recorded", "mixed"]);
    let s = serde_json::to_string(&env).unwrap();
    assert!(
        s.contains("\"code\":\"invalid_network_mode\""),
        "envelope: {s}"
    );
    assert!(s.contains("\"provided\":\"bogus\""), "envelope: {s}");
    assert_eq!(env.code, LoomErrorCode::InvalidNetworkMode);
}

#[test]
fn from_invalid_budget_key_serialises_correctly() {
    let env = ErrorTranslator::from_invalid_budget_key(
        "garbage",
        &["network", "wall_clock", "dom_nodes", "js_heap"],
    );
    let s = serde_json::to_string(&env).unwrap();
    assert!(
        s.contains("\"code\":\"invalid_budget_key\""),
        "envelope: {s}"
    );
    assert!(s.contains("\"provided\":\"garbage\""), "envelope: {s}");
    assert_eq!(env.code, LoomErrorCode::InvalidBudgetKey);
}

#[test]
fn typed_validation_variants_serialise_as_snake_case() {
    let s = serde_json::to_string(&LoomErrorCode::UnknownProfile).unwrap();
    assert_eq!(s, "\"unknown_profile\"");
    let s = serde_json::to_string(&LoomErrorCode::InvalidNetworkMode).unwrap();
    assert_eq!(s, "\"invalid_network_mode\"");
    let s = serde_json::to_string(&LoomErrorCode::InvalidBudgetKey).unwrap();
    assert_eq!(s, "\"invalid_budget_key\"");
}

#[test]
fn translator_is_stateless_function_namespace() {
    // No constructor — pure function namespace (single
    // conversion point; no per-instance state).
    let _: fn(SchemaViolationDetail) -> JsonRpcError = ErrorTranslator::from_schema_violation;
    let _: fn(&str) -> String = ErrorTranslator::truncate_message;
}
