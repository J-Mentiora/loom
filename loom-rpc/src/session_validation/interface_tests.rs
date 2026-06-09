//! Interface tests for `session_validation::validate_create_session_params`.
//! Covers behavior at the function level (handler-level
//! coverage lives in `rpc_handlers/interface_tests.rs`; integration-level
//! coverage in `tests/ac_profval.rs`).

use super::session_validation::validate_create_session_params;
use crate::core_service_adapter::core_service_adapter::CreateSessionParams;
use crate::error_translator::error_translator::LoomErrorCode;

fn params(
    profile: &str,
    network_mode: &str,
    budget: Option<serde_json::Value>,
) -> CreateSessionParams {
    CreateSessionParams {
        profile: profile.to_string(),
        network_mode: network_mode.to_string(),
        capture_policy: None,
        seed: None,
        clock_anchor: None,
        budget,
        no_blocklist: false,
        no_determinism: false,
    }
}

fn params_with_capture_policy(
    profile: &str,
    network_mode: &str,
    capture_policy: Option<&str>,
) -> CreateSessionParams {
    CreateSessionParams {
        profile: profile.to_string(),
        network_mode: network_mode.to_string(),
        capture_policy: capture_policy.map(|s| s.to_string()),
        seed: None,
        clock_anchor: None,
        budget: None,
        no_blocklist: false,
        no_determinism: false,
    }
}

#[test]
fn validate_accepts_canonical_inputs() {
    let p = params("safe", "live", None);
    assert!(validate_create_session_params(&p).is_ok());
}

#[test]
fn validate_accepts_canonical_with_canonical_budget_keys() {
    // KNOWN_BUDGET_KEYS holds the user-facing CLI keys
    // (network/wall_clock/dom_nodes/js_heap). Direct-RPC clients send
    // the same canonical keys; the typed BudgetLimits wire shape used
    // by the CLI's `parse_budget_string` is rejected by name (those
    // typed field names aren't in KNOWN_BUDGET_KEYS) — that's fine,
    // because the CLI pre-validates before sending.
    let p = params(
        "standard",
        "recorded",
        Some(serde_json::json!({"network": "10MB", "wall_clock": "30s"})),
    );
    assert!(validate_create_session_params(&p).is_ok());
}

#[test]
fn validate_rejects_unknown_profile() {
    let p = params("nonexistent", "live", None);
    let err = validate_create_session_params(&p).expect_err("must reject");
    assert_eq!(err.code, LoomErrorCode::UnknownProfile);
    let data = err.data.expect("must carry data");
    assert_eq!(data["provided"], "nonexistent");
    assert!(data["available"].is_array());
}

#[test]
fn validate_rejects_invalid_network_mode() {
    let p = params("safe", "bogus", None);
    let err = validate_create_session_params(&p).expect_err("must reject");
    assert_eq!(err.code, LoomErrorCode::InvalidNetworkMode);
    let data = err.data.expect("must carry data");
    assert_eq!(data["provided"], "bogus");
}

#[test]
fn validate_rejects_invalid_budget_key() {
    let p = params("safe", "live", Some(serde_json::json!({"garbage": 5})));
    let err = validate_create_session_params(&p).expect_err("must reject");
    assert_eq!(err.code, LoomErrorCode::InvalidBudgetKey);
    let data = err.data.expect("must carry data");
    assert_eq!(data["provided"], "garbage");
}

#[test]
fn validate_skips_non_object_budget() {
    // Non-object budget shapes (e.g. accidental string) fall through;
    // schema / typing layer handles them. Validator does not panic.
    let p = params(
        "safe",
        "live",
        Some(serde_json::Value::String("not-an-object".into())),
    );
    assert!(validate_create_session_params(&p).is_ok());
}

/// the CLI parses `--budget wall_clock=1s` into a
/// `BudgetLimits` struct that serialises to JSON using its typed
/// internal field names (`session_walltime_ms`, etc.). The RPC
/// validator must accept that shape — a regression here breaks every
/// `loom session create --budget ...` invocation.
#[test]
fn validate_accepts_typed_budget_limits_field_names() {
    // CLI-serialised BudgetLimits — the actual wire shape today.
    let p = params(
        "safe",
        "live",
        Some(serde_json::json!({
            "session_walltime_ms": 1000,
            "action_walltime_ms": 60000,
            "network_bytes": 10_000_000,
            "dom_nodes": 50000,
            "js_heap_bytes": 536870912,
        })),
    );
    assert!(
        validate_create_session_params(&p).is_ok(),
        "CLI-serialised BudgetLimits with typed field names must pass validation"
    );
}

/// direct-RPC clients sending the user-facing keys
/// (network / wall_clock / dom_nodes / js_heap) still work.
#[test]
fn validate_accepts_user_facing_budget_keys() {
    let p = params(
        "safe",
        "live",
        Some(serde_json::json!({
            "wall_clock": "30s",
            "network": "10MB",
            "dom_nodes": 1000,
            "js_heap": "256MB",
        })),
    );
    assert!(validate_create_session_params(&p).is_ok());
}

#[test]
fn validate_rejects_profile_before_network_mode() {
    // Order: profile is checked first.
    let p = params("nonexistent", "bogus", None);
    let err = validate_create_session_params(&p).expect_err("must reject");
    assert_eq!(err.code, LoomErrorCode::UnknownProfile);
}

#[test]
fn validate_rejects_network_mode_before_budget() {
    // Order: network_mode is checked before budget.
    let p = params("safe", "bogus", Some(serde_json::json!({"garbage": 5})));
    let err = validate_create_session_params(&p).expect_err("must reject");
    assert_eq!(err.code, LoomErrorCode::InvalidNetworkMode);
}

// === server-side capture_policy validation ===

#[test]
fn unknown_capture_policy_returns_typed_invalid_capture_policy() {
    let p = params_with_capture_policy("safe", "live", Some("bogus"));
    let err = validate_create_session_params(&p).expect_err("must reject");
    assert_eq!(err.code, LoomErrorCode::InvalidCapturePolicy);
    let data = err.data.expect("must carry data");
    assert_eq!(data["provided"], "bogus");
    assert!(data["available"].is_array());
    let avail = data["available"].as_array().expect("array");
    let strs: Vec<&str> = avail.iter().filter_map(|v| v.as_str()).collect();
    assert!(strs.contains(&"minimal"));
    assert!(strs.contains(&"default"));
    assert!(strs.contains(&"full"));
}

#[test]
fn valid_capture_policy_passes() {
    for v in ["minimal", "default", "full"] {
        let p = params_with_capture_policy("safe", "live", Some(v));
        assert!(
            validate_create_session_params(&p).is_ok(),
            "expected {v} to validate"
        );
    }
}

#[test]
fn absent_capture_policy_passes() {
    let p = params_with_capture_policy("safe", "live", None);
    assert!(validate_create_session_params(&p).is_ok());
}
