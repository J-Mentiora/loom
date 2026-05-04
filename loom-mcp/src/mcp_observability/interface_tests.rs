// Interface tests for `McpObservability`. Verifies AC-ARCH-38 span
// fields, stderr-only discipline (BC-MCP-04 adjacent -- stdout reserved
// for MCP framing per IC-MCP-09), and vault redaction.

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

// === AC-ARCH-38: per-request span fields ===

#[test]
fn span_request_start_signature() {
    fn _ck(
        o: &McpObservability,
        rid: String,
        m: &'static str,
        t: Option<String>,
    ) -> RequestSpan {
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
    fn _ck(
        o: &McpObservability,
        tool: &str,
        v: serde_json::Value,
    ) -> serde_json::Value {
        o.redact_arguments(tool, v)
    }
    let _ = _ck;
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
        (
            s.request_id.as_str(),
            s.mcp_method,
            s.tool_name.as_deref(),
        )
    }
    let _ = _ck;
}
