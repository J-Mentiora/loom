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

/// Tool names whose `arguments` fields are redacted when `redact_vault: true`.
pub const REDACTED_TOOL_NAMES: &[&str] = &["loom.vault.grant", "loom.vault.revoke"];

/// `Duration` → microseconds, saturating on overflow.
pub fn duration_to_us(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}
