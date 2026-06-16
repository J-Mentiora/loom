pub mod error_mapper;
pub use error_mapper::*;

#[cfg(test)]
mod interface_tests;

use loom_rpc::error::{LoomError, LoomErrorCode};

/// Whether a request had been dispatched to the daemon at the point a transport
/// fault occurred — gates whether `RpcClient::call` may auto-retry a
/// non-idempotent verb. See [`ErrorMapper::from_transport_dropped`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchPhase {
    /// The send never completed (broken pipe on write, or no connection) — the
    /// request provably did not reach the daemon, so retry is a fresh first
    /// attempt, safe for ANY verb.
    Pre,
    /// The send completed but the read failed (EOF/closed) — the request may
    /// have executed, so only idempotent verbs may be auto-retried.
    Post,
}

impl DispatchPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            DispatchPhase::Pre => "pre",
            DispatchPhase::Post => "post",
        }
    }
}

impl ErrorMapper {
    pub fn to_tool_result(err: LoomError) -> ToolResult {
        let msg = truncate_chars(&err.message, MAX_MESSAGE_CHARS);
        // Start `data` from the error's structured context so MCP callers get the
        // typed fields the daemon attached — `session_cap_exceeded`'s
        // {active, cap, hint}, `profile_restricted`'s {matched_pattern, profile,
        // violation}, etc. — structurally, not just buried in `message`. Then
        // overlay retryability (`retryable`/`retry`) so callers can still
        // distinguish an infra flake from a real page error. The overlay wins on
        // a key clash, but the daemon's context keys never collide with these.
        let mut data_obj = match err.context {
            Some(serde_json::Value::Object(map)) => map,
            _ => serde_json::Map::new(),
        };
        if let Some(retry) = match err.code.retry_disposition() {
            loom_rpc::error::RetryDisposition::None => None,
            loom_rpc::error::RetryDisposition::Reconnect => Some("reconnect"),
            loom_rpc::error::RetryDisposition::Backoff => Some("backoff"),
        } {
            data_obj.insert("retryable".to_string(), serde_json::Value::Bool(true));
            data_obj.insert(
                "retry".to_string(),
                serde_json::Value::String(retry.to_string()),
            );
        }
        let data = if data_obj.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(data_obj))
        };
        let receipt = TypedReceipt {
            code: err.code.as_wire().to_string(),
            message: msg,
            data,
            remediation: remediation_for(err.code),
        };
        ToolResult {
            is_error: true,
            content: vec![McpContent::from_json(
                serde_json::to_value(&receipt).unwrap_or(serde_json::Value::Null),
            )],
        }
    }

    pub fn typed_receipt(code: LoomErrorCode) -> TypedReceipt {
        TypedReceipt {
            code: code.as_wire().to_string(),
            message: String::new(),
            data: None,
            remediation: remediation_for(code),
        }
    }

    pub fn from_rpc_io(detail: &str) -> LoomError {
        LoomError::new(LoomErrorCode::Io, detail)
    }

    /// A transient transport/connection fault on the daemon socket — broken
    /// pipe, connection closed/EOF, daemon idle-drop. Typed as
    /// `TransportDropped` (retryable via reconnect) instead of opaque `io`.
    ///
    /// `phase` records whether the request was provably-not-dispatched
    /// (`"pre"` — the send never completed, so a retry is a fresh first attempt
    /// safe for ANY verb) or possibly-dispatched (`"post"` — the failure was on
    /// the read after a successful send, so only idempotent verbs may be
    /// auto-retried). `RpcClient::call` reads this to gate the retry. The
    /// message is generic (no internal paths/tokens) per the security invariant.
    pub fn from_transport_dropped(detail: &str, phase: DispatchPhase) -> LoomError {
        LoomError::new(LoomErrorCode::TransportDropped, detail)
            .with_context(serde_json::json!({ "dispatch_phase": phase.as_str() }))
    }

    /// The daemon returned an error envelope we could not decode into a typed
    /// error. This means the request WAS processed (the daemon produced a
    /// response), so it must NOT be retried — classify as a non-retryable
    /// protocol error rather than transport. With the tolerant `from_wire`
    /// decode this is now rare (only a genuinely malformed envelope hits it).
    pub fn from_malformed_response() -> LoomError {
        LoomError::new(
            LoomErrorCode::RpcInvalidRequest,
            "malformed error response from daemon",
        )
    }

    pub fn from_hello_mismatch(detail: &str) -> LoomError {
        LoomError::new(LoomErrorCode::RpcAuthFailed, detail)
    }

    pub fn from_unknown_tool(tool_name: &str) -> LoomError {
        LoomError::new(
            LoomErrorCode::InvalidArgument,
            format!("unknown tool: {tool_name}"),
        )
    }

    pub fn from_schema_parse(detail: &str) -> LoomError {
        LoomError::new(LoomErrorCode::RpcInvalidRequest, detail)
    }
}

impl From<LoomError> for ToolResult {
    fn from(err: LoomError) -> Self {
        ErrorMapper::to_tool_result(err)
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    let mut chars = s.chars();
    let taken: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{taken}\u{2026}")
    } else {
        taken
    }
}

fn remediation_for(_code: LoomErrorCode) -> Option<String> {
    None
}
