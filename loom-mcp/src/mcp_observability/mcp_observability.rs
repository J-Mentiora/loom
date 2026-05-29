// McpObservability — tracing JSON to stderr with a vault-redaction layer.
// Types only. Implementation in mod.rs.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Outcome enum for the `outcome` span field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok,
    Error,
    DaemonDisconnected,
    ClientCancelled,
}

/// Opaque span handle returned by `span_request_start`.
#[derive(Debug)]
pub struct RequestSpan {
    pub(crate) request_id: String,
    pub(crate) mcp_method: &'static str,
    pub(crate) tool_name: Option<String>,
    pub(crate) started_at: std::time::Instant,
}

/// The McpObservability sink.
pub struct McpObservability {
    pub(crate) redact_vault: bool,
}

/// Tool names whose `arguments` fields are redacted **wholesale** to
/// `"[REDACTED]"` when `redact_vault: true`. Used for vault management
/// tools where the argument payload itself is the credential material.
pub const REDACTED_TOOL_NAMES: &[&str] = &["loom.vault.grant", "loom.vault.revoke"];

/// v0.9.6 web-cookie-injection: tool names whose arguments + results
/// need **path-level** value redaction (rather than wholesale) when
/// `redact_vault: true`. Cookie names + structure remain visible —
/// only `value` fields are stripped. Reuses the same `redact_vault`
/// toggle as OAuth tokens because cookie values share the credential
/// family per D11.
pub const COOKIE_REDACTED_TOOL_NAMES: &[&str] = &[
    "loom.web.set_cookies",
    "loom.web.get_cookies",
    "loom.web.delete_cookies",
    // clear_cookies has no value field on the wire — included for
    // tool-name lookup symmetry; no-op in practice.
    "loom.web.clear_cookies",
];

/// JSONPath-style redaction targets applied to cookie tool payloads:
///   - `$.params.source.cookies[*].value`  — set_cookies inline source
///   - `$.params.cookies[*].value`          — set_cookies legacy flat
///   - `$.result.get_cookies_result[*].value` — get_cookies response
/// Intentionally NOT redacted:
///   - `$.result.set_cookies_result[*].error_code` (taxonomy strings only)
pub fn redact_cookie_paths_in_place(v: &mut serde_json::Value) {
    fn redact_value_field_in_array(arr: &mut Vec<serde_json::Value>) {
        for item in arr.iter_mut() {
            if let Some(obj) = item.as_object_mut() {
                if obj.contains_key("value") {
                    obj.insert(
                        "value".to_string(),
                        serde_json::Value::String("[REDACTED]".to_string()),
                    );
                }
            }
        }
    }

    fn walk(v: &mut serde_json::Value) {
        if let Some(obj) = v.as_object_mut() {
            // Direct array at this level — redact value fields inside.
            for (k, sub) in obj.iter_mut() {
                // Cookie-bearing arrays: "cookies" (set_cookies input),
                // "*_cookies_result" (verb output arrays). For
                // simplicity, match by suffix/keyword.
                if (k == "cookies" || k.ends_with("_cookies_result"))
                    && sub.is_array()
                {
                    if let Some(arr) = sub.as_array_mut() {
                        redact_value_field_in_array(arr);
                    }
                }
                // Recurse into nested objects/arrays to reach
                // $.params.source.cookies[*].value etc.
                walk(sub);
            }
        } else if let Some(arr) = v.as_array_mut() {
            for item in arr.iter_mut() {
                walk(item);
            }
        }
    }

    walk(v);
}

/// `Duration` → microseconds, saturating on overflow.
pub fn duration_to_us(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}
