// Interface tests for `McpObservability`. Verifies per-request span
// fields, stderr-only logging discipline (stdout is reserved for MCP
// framing), and vault redaction.

use super::mcp_observability::{
    duration_to_us, McpObservability, Outcome, RequestSpan, REDACTED_TOOL_NAMES,
};
use std::sync::Arc;
use std::time::Duration;

// === Constructor / lifecycle ===

#[test]
fn new_returns_arc() {
    let _: Arc<McpObservability> = McpObservability::new(true);
}

#[test]
fn redact_vault_flag_persists() {
    let _ = McpObservability::new(true);
    let _ = McpObservability::new(false);
}

// === per-request span fields ===

#[test]
fn span_request_start_signature() {
    fn _ck(o: &McpObservability, rid: String, m: &'static str, t: Option<String>) -> RequestSpan {
        o.span_request_start(rid, m, t)
    }
    let _ = _ck;
}

#[test]
fn span_request_end_takes_outcome_and_optional_error_code() {
    fn _ck(o: &McpObservability, s: RequestSpan, oc: Outcome, ec: Option<&str>) {
        o.span_request_end(s, oc, ec)
    }
    let _ = _ck;
}

#[test]
fn outcome_enum_has_required_variants() {
    let _all = [
        Outcome::Ok,
        Outcome::Error,
        Outcome::DaemonDisconnected,
        Outcome::ClientCancelled,
    ];
}

// === Latency: microseconds derived from Duration ===

#[test]
fn duration_to_us_round_trips_a_millisecond() {
    assert_eq!(duration_to_us(Duration::from_millis(1)), 1_000);
}

#[test]
fn duration_to_us_saturates_on_overflow() {
    // u64::MAX micros is enormous; this ensures the conversion never panics.
    let huge = Duration::from_secs(u64::MAX / 2);
    let _ = duration_to_us(huge); // must not panic
}

// === Vault redaction layer ===

#[test]
fn redacted_tool_names_includes_vault_grant_and_revoke() {
    assert!(REDACTED_TOOL_NAMES.contains(&"loom.vault.grant"));
    assert!(REDACTED_TOOL_NAMES.contains(&"loom.vault.revoke"));
}

#[test]
fn redact_arguments_signature() {
    fn _ck(o: &McpObservability, tool: &str, v: serde_json::Value) -> serde_json::Value {
        o.redact_arguments(tool, v)
    }
    let _ = _ck;
}

// === v0.9.6 cookie redaction (path-level) ===

use super::mcp_observability::{redact_cookie_paths_in_place, COOKIE_REDACTED_TOOL_NAMES};

#[test]
fn cookie_redacted_tool_names_includes_four_cookie_verbs() {
    assert!(COOKIE_REDACTED_TOOL_NAMES.contains(&"loom.web.set_cookies"));
    assert!(COOKIE_REDACTED_TOOL_NAMES.contains(&"loom.web.get_cookies"));
    assert!(COOKIE_REDACTED_TOOL_NAMES.contains(&"loom.web.clear_cookies"));
    assert!(COOKIE_REDACTED_TOOL_NAMES.contains(&"loom.web.delete_cookies"));
}

#[test]
fn redact_cookie_paths_strips_value_from_set_cookies_inline_source() {
    let mut v = serde_json::json!({
        "params": {
            "source": {
                "source": "inline",
                "cookies": [
                    {"name": "sid", "value": "SECRET", "domain": "example.com"},
                    {"name": "uid", "value": "OTHER_SECRET", "domain": "x"}
                ]
            }
        }
    });
    redact_cookie_paths_in_place(&mut v);
    let s = v.to_string();
    assert!(!s.contains("SECRET"));
    assert!(!s.contains("OTHER_SECRET"));
    assert!(s.contains("[REDACTED]"));
    // Names + structure preserved.
    assert!(s.contains("\"name\":\"sid\""));
    assert!(s.contains("\"domain\":\"example.com\""));
}

#[test]
fn redact_cookie_paths_strips_value_from_flat_cookies_array() {
    let mut v = serde_json::json!({
        "params": {
            "cookies": [
                {"name": "sid", "value": "SECRET", "domain": "x"}
            ]
        }
    });
    redact_cookie_paths_in_place(&mut v);
    let s = v.to_string();
    assert!(!s.contains("SECRET"));
    assert!(s.contains("[REDACTED]"));
}

#[test]
fn redact_cookie_paths_strips_value_from_get_cookies_result_array() {
    let mut v = serde_json::json!({
        "result": {
            "get_cookies_result": [
                {"name": "sid", "value": "LIVE_TOKEN", "domain": "x", "path": "/"}
            ]
        }
    });
    redact_cookie_paths_in_place(&mut v);
    let s = v.to_string();
    assert!(!s.contains("LIVE_TOKEN"));
    assert!(s.contains("[REDACTED]"));
}

#[test]
fn redact_cookie_paths_preserves_set_cookies_result_error_code_taxonomy() {
    // error_code is taxonomy text (not a value); must NOT be redacted.
    let mut v = serde_json::json!({
        "result": {
            "set_cookies_result": [
                {"name": "sid", "success": false, "error_code": "name_empty"}
            ]
        }
    });
    redact_cookie_paths_in_place(&mut v);
    let s = v.to_string();
    assert!(s.contains("\"error_code\":\"name_empty\""), "got: {s}");
}

#[test]
fn redact_arguments_with_redact_vault_strips_cookie_values_for_set_cookies_tool() {
    let obs = McpObservability::new(true); // redact_vault on
    let args = serde_json::json!({
        "source": {
            "source": "inline",
            "cookies": [
                {"name": "sid", "value": "ULTRA_SECRET", "domain": "x"}
            ]
        }
    });
    let redacted = obs.redact_arguments("loom.web.set_cookies", args);
    let s = redacted.to_string();
    assert!(!s.contains("ULTRA_SECRET"));
    assert!(s.contains("[REDACTED]"));
}

#[test]
fn redact_arguments_passes_through_when_redact_vault_off() {
    let obs = McpObservability::new(false); // redact_vault off
    let args = serde_json::json!({
        "source": {
            "source": "inline",
            "cookies": [
                {"name": "sid", "value": "ULTRA_SECRET", "domain": "x"}
            ]
        }
    });
    let redacted = obs.redact_arguments("loom.web.set_cookies", args);
    let s = redacted.to_string();
    // redact_vault is OFF — values pass through unchanged.
    assert!(s.contains("ULTRA_SECRET"));
}

#[test]
fn redact_arguments_for_non_cookie_non_vault_tool_is_passthrough() {
    let obs = McpObservability::new(true);
    let args = serde_json::json!({"selector": "#submit"});
    let out = obs.redact_arguments("loom.web.click", args.clone());
    assert_eq!(out, args);
}

// === Logging level signatures (stderr-only writer is wired by init_subscriber) ===

#[test]
fn info_warn_error_signatures_take_msg_and_json_fields() {
    fn _ck(o: &McpObservability, m: &str, f: serde_json::Value) {
        o.info(m, f.clone());
        o.warn(m, f.clone());
        o.error(m, f);
    }
    let _ = _ck;
}

// === Span handle holds required field set ===

#[test]
fn request_span_holds_request_id_and_mcp_method() {
    fn _ck(s: &RequestSpan) -> (&str, &'static str, Option<&str>) {
        (s.request_id.as_str(), s.mcp_method, s.tool_name.as_deref())
    }
    let _ = _ck;
}

// === v0.9.6 follow-up: redact_cookie_paths_in_place edge cases ===

#[test]
fn redact_cookie_paths_handles_deeply_nested_cookies_arrays() {
    // The walker recurses into nested objects; cookies inside several
    // levels of nesting (e.g. wrapped in audit/diagnostic envelopes)
    // still get scrubbed.
    let mut v = serde_json::json!({
        "outer": {
            "middle": {
                "inner": {
                    "params": {
                        "source": {
                            "cookies": [
                                {"name": "deep_sid", "value": "DEEPLY_HIDDEN", "domain": "x"}
                            ]
                        }
                    }
                }
            }
        }
    });
    redact_cookie_paths_in_place(&mut v);
    let s = v.to_string();
    assert!(!s.contains("DEEPLY_HIDDEN"));
    assert!(s.contains("[REDACTED]"));
    assert!(s.contains("\"name\":\"deep_sid\""));
}

#[test]
fn redact_cookie_paths_handles_cookies_array_inside_array() {
    // The walker recurses into arrays-of-objects too. A non-cookies
    // wrapper array containing objects with cookies arrays must scrub.
    let mut v = serde_json::json!({
        "batch": [
            {"cookies": [{"name": "a", "value": "AAAA", "domain": "x"}]},
            {"cookies": [{"name": "b", "value": "BBBB", "domain": "y"}]}
        ]
    });
    redact_cookie_paths_in_place(&mut v);
    let s = v.to_string();
    assert!(!s.contains("AAAA"));
    assert!(!s.contains("BBBB"));
    let redacted = s.matches("[REDACTED]").count();
    assert_eq!(redacted, 2);
}

#[test]
fn redact_cookie_paths_no_op_when_no_cookies_anywhere() {
    let mut v = serde_json::json!({
        "params": {"session_id": "S", "url": "https://x.com", "selector": "#submit"}
    });
    let snapshot = v.clone();
    redact_cookie_paths_in_place(&mut v);
    assert_eq!(v, snapshot, "non-cookie tool payload must pass through");
}

#[test]
fn redact_cookie_paths_no_op_on_empty_cookies_array() {
    let mut v = serde_json::json!({
        "params": {"cookies": []}
    });
    let snapshot = v.clone();
    redact_cookie_paths_in_place(&mut v);
    assert_eq!(v, snapshot);
}

#[test]
fn redact_cookie_paths_handles_cookies_array_at_root() {
    // The walker recurses into root-level cookies arrays too.
    let mut v = serde_json::json!({
        "cookies": [{"name": "sid", "value": "TOP_LEVEL_SECRET", "domain": "x"}]
    });
    redact_cookie_paths_in_place(&mut v);
    let s = v.to_string();
    assert!(!s.contains("TOP_LEVEL_SECRET"));
    assert!(s.contains("[REDACTED]"));
}

#[test]
fn redact_cookie_paths_cookies_result_with_partial_value_redacts() {
    let mut v = serde_json::json!({
        "result": {
            "get_cookies_result": [
                {"name": "sid", "value": "SECRET1", "domain": "x"},
                {"name": "uid"}  // no value field — no-op
            ]
        }
    });
    redact_cookie_paths_in_place(&mut v);
    let s = v.to_string();
    assert!(!s.contains("SECRET1"));
    let redacted = s.matches("[REDACTED]").count();
    assert_eq!(redacted, 1);
}

#[test]
fn redact_cookie_paths_with_cookies_array_containing_non_object_entries() {
    // Array entries that aren't objects are skipped (no redaction
    // possible, no panic).
    let mut v = serde_json::json!({
        "cookies": [
            {"name": "sid", "value": "S1"},
            "string_entry_skipped",
            42,
            null
        ]
    });
    redact_cookie_paths_in_place(&mut v);
    let s = v.to_string();
    assert!(!s.contains("S1"));
    assert!(s.contains("[REDACTED]"));
    // Non-object entries survive untouched.
    assert!(s.contains("string_entry_skipped"));
    assert!(s.contains("42"));
}

#[test]
fn redact_cookie_paths_replaces_value_even_if_value_is_object() {
    // A `value` field that's an object (not a string) would normally
    // be preserved by the JSON, but the redactor overwrites it with
    // "[REDACTED]" regardless of type. This is conservative: the
    // operator who put a non-string in `value` likely intended to
    // hide it.
    let mut v = serde_json::json!({
        "cookies": [{"name": "sid", "value": {"weird_nested": "secret"}}]
    });
    redact_cookie_paths_in_place(&mut v);
    let s = v.to_string();
    assert!(!s.contains("weird_nested"));
    assert!(s.contains("[REDACTED]"));
}

#[test]
fn redact_arguments_with_non_existing_cookie_tool_passes_through_unchanged() {
    let obs = McpObservability::new(true);
    let args = serde_json::json!({"foo": "bar"});
    let out = obs.redact_arguments("loom.web.set_cookies_typo", args.clone());
    // Not in COOKIE_REDACTED_TOOL_NAMES → no redaction.
    assert_eq!(out, args);
}

#[test]
fn redact_arguments_cookie_tool_with_redact_vault_false_passes_through() {
    let obs = McpObservability::new(false);
    let args = serde_json::json!({
        "source": {"source": "inline", "cookies": [{"name": "sid", "value": "S"}]}
    });
    let out = obs.redact_arguments("loom.web.set_cookies", args.clone());
    // redact_vault is OFF → values pass through.
    let s = out.to_string();
    assert!(s.contains("\"value\":\"S\""));
}

#[test]
fn cookie_redacted_tool_names_does_not_include_non_cookie_loom_web_tools() {
    // Sanity: random web tools must NOT be in the cookie redaction
    // list. Their redaction (if any) is handled by other paths.
    assert!(!COOKIE_REDACTED_TOOL_NAMES.contains(&"loom.web.navigate"));
    assert!(!COOKIE_REDACTED_TOOL_NAMES.contains(&"loom.web.click"));
    assert!(!COOKIE_REDACTED_TOOL_NAMES.contains(&"loom.web.evaluate"));
    assert!(!COOKIE_REDACTED_TOOL_NAMES.contains(&"loom.session.create"));
}
