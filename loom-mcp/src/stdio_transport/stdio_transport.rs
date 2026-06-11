// StdioTransport — NDJSON framing over stdin/stdout.
// Types only. Implementation in mod.rs.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// One inbound MCP request frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpRequest {
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// One outbound MCP response frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResponse {
    pub jsonrpc: String,
    pub id: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<McpProtocolError>,
}

/// Protocol-level error envelope (NOT for domain errors — those use ToolResult).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpProtocolError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Maximum number of concurrently dispatched in-flight requests. Bounds
/// the per-request task spawn (a frame flood can't grow unbounded tasks);
/// at capacity the read loop pauses until a slot frees. Small but
/// comfortably above what an interactive MCP client pipelines.
pub const MAX_CONCURRENT_REQUESTS: usize = 32;

pub const ERROR_PARSE: i32 = -32700;
pub const ERROR_INVALID_REQUEST: i32 = -32600;
pub const ERROR_METHOD_NOT_FOUND: i32 = -32601;
pub const ERROR_INVALID_PARAMS: i32 = -32602;
pub const ERROR_INTERNAL: i32 = -32603;

/// Dispatch closure invoked once per inbound frame.
pub type Dispatch = Arc<
    dyn Fn(McpRequest) -> futures::future::BoxFuture<'static, Option<McpResponse>> + Send + Sync,
>;

/// Wire method name for an MCP tool invocation. Mirrors
/// `mcp_dispatcher::METHOD_TOOLS_CALL`; duplicated here so the transport's
/// ordering classifier stays decoupled from the dispatcher module.
const METHOD_TOOLS_CALL: &str = "tools/call";

/// Wire tool name of the read-only implicit-session inspector. Mirrors
/// `mcp_dispatcher::TOOL_SESSION_INFO`; the only `loom.session.*` tool that
/// does not mutate session order, so it stays on the concurrent lane.
const TOOL_SESSION_INFO_NAME: &str = "loom.session.info";

impl McpRequest {
    /// Whether this request mutates the implicit browser session and so
    /// must execute in strict submission order, or is a control-plane
    /// request that may dispatch concurrently.
    ///
    /// ORDERED (session-mutating; FIFO, one at a time): the implicit MCP
    /// session is ONE browser — two browser actions can't run at once and
    /// they MUST execute in the order the client sent them. A `navigate`
    /// followed by an `evaluate` that raced ahead would read the
    /// pre-navigate page (the v0.11.0-blocking regression: PR #162 spawned
    /// every frame on its own task, letting the later action win). Ordered
    /// requests are `tools/call` for the `loom.web.*` family (every page
    /// action) and the session-mutating server-local session tools
    /// (`loom.session.reset` closes + recreates the session).
    ///
    /// A `tools/call` whose tool name we cannot positively classify as
    /// read-only is treated as ORDERED — the safe default: serializing a
    /// read-only call only costs a little latency, but concurrently running
    /// a mutating one corrupts session order.
    ///
    /// CONCURRENT (control-plane; may overtake a slow action — #162's
    /// legitimate goal so `ping`/`initialize` aren't head-of-line blocked):
    /// every non-`tools/call` method (`initialize`, `ping`, `shutdown`,
    /// `notifications/*`, `tools/list`, `resources/list`, `resources/read`,
    /// `prompts/list`, and any unknown method — rejected without touching
    /// session state), plus `tools/call` for `loom.session.info`, the one
    /// server-local session tool that is read-only (it only reads /
    /// lazily-creates session metadata; creation is mutex-idempotent in the
    /// dispatcher).
    ///
    /// Notifications (no `id`) yield no response and never touch session
    /// state, so they ride the concurrent lane too.
    pub fn is_session_ordered(&self) -> bool {
        if self.method != METHOD_TOOLS_CALL {
            return false;
        }
        let Some(name) = self.params.get("name").and_then(|v| v.as_str()) else {
            // Malformed `tools/call` (no name): order it so it can't race a
            // real action; the dispatcher rejects it as an unknown tool.
            return true;
        };
        // `loom.session.info` is the sole read-only session tool; every
        // other tools/call name (loom.web.*, the mutating session tools,
        // and any unknown name) is ordered.
        name != TOOL_SESSION_INFO_NAME
    }
}

/// Transport over generic AsyncRead/AsyncWrite.
pub struct StdioTransport<R, W> {
    pub(crate) reader: R,
    pub(crate) writer: W,
}
