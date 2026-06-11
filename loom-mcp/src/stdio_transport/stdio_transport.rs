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

/// Transport over generic AsyncRead/AsyncWrite.
pub struct StdioTransport<R, W> {
    pub(crate) reader: R,
    pub(crate) writer: W,
}
