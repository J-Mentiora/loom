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
        // Surface retryability so MCP callers (e.g. the studio) can distinguish
        // an infra flake from a real page error. `data.retryable` /
        // `data.retry` carry the disposition; the success return shapes of the
        // `web.*` verbs are unchanged (this only enriches the error envelope).
        let data = match err.code.retry_disposition() {
            loom_rpc::error::RetryDisposition::None => None,
            disp => Some(serde_json::json!({
                "retryable": true,
                "retry": match disp {
                    loom_rpc::error::RetryDisposition::Reconnect => "reconnect",
                    loom_rpc::error::RetryDisposition::Backoff => "backoff",
                    loom_rpc::error::RetryDisposition::None => "none",
                },
            })),
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
