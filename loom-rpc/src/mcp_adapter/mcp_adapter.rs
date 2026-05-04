// Re-export of the locked Phase 5.3 interface. DO NOT EDIT here.
// Edit `systems/loom-rpc/modules/mcp_adapter/interfaces.rs` instead.
// McpAdapter — MCP wire-protocol translator (UX-13 / FR-PROTO-03 /
// AC-PROTO-03.1).
//
// # Contract semantics
// - **1:1 tool ↔ method mapping (UX-13).** MCP tool names are equal
//   to the canonical JSON-RPC method names (`session.create`,
//   `action.web.click`, ...). Tool descriptions + input schemas are
//   pulled from `SchemaProvider::get_registry_snapshot` so the MCP
//   tool list is bit-equal to the RPC method list at startup.
// - **No bypass (module_list.md note).** McpAdapter does NOT bypass
//   `RequestRouter`. Every translated `tools/call` is dispatched
//   through the same `RequestRouter::dispatch` path so validation,
//   observability, and error translation behave identically.
// - **Receipt → ToolResult.** Successful RPC responses are wrapped
//   as MCP `ToolResult.content` (text content with the canonical
//   JSON receipt). Errors are wrapped as `ToolResult { isError: true,
//   content: [{type: "text", text: <serialised JsonRpcError>}] }`.
// - **Stdio transport.** The adapter is invoked by `loom mcp serve`
//   on stdin/stdout; it does NOT bind a Unix socket. Auth is handled
//   by the parent `loom mcp serve` process at spawn time, not by
//   this adapter (MCP clients are local subprocesses).

use crate::error_translator::error_translator::JsonRpcError;
use crate::request_router::request_router::RequestRouterApi;
use crate::schema_provider::schema_provider::SchemaProviderApi;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// MCP `tools/list` entry shape (subset relevant to this adapter).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// MCP `tools/call` request shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

/// MCP `ToolResult` shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolResult {
    pub content: Vec<McpContent>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_error: bool,
}

fn is_false(b: &bool) -> bool {
    !b
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpContent {
    Text { text: String },
}

#[derive(Debug)]
pub enum McpError {
    /// MCP frame did not parse as JSON-RPC 2.0.
    Malformed,
    /// Tool name does not exist in the registry.
    UnknownTool { name: String },
    /// Underlying RPC dispatch produced a JSON-RPC error envelope.
    Rpc(JsonRpcError),
}

#[allow(dead_code)]
pub struct McpAdapter {
    pub(crate) router: Arc<dyn RequestRouterApi>,
    pub(crate) schemas: Arc<dyn SchemaProviderApi>,
}

#[derive(Debug)]
pub struct ParityViolation {
    pub missing_in_mcp: Vec<String>,
    pub missing_in_rpc: Vec<String>,
}
