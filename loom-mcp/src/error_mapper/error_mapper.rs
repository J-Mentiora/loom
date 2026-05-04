// ErrorMapper — single conversion point from `LoomError` to MCP
// `ToolResult { isError: true, content }`.
//
// Types only. Implementation in mod.rs.

use serde::{Deserialize, Serialize};

/// MCP content block carried inside a `ToolResult`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpContent {
    Json { json: serde_json::Value },
    Text { text: String },
}

/// MCP `ToolResult` — the response shape for `tools/call`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolResult {
    #[serde(rename = "isError")]
    pub is_error: bool,
    pub content: Vec<McpContent>,
}

/// Typed receipt body inside the error content block.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TypedReceipt {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

/// Maximum length of `TypedReceipt::message` (chars, not bytes).
pub const MAX_MESSAGE_CHARS: usize = 280;

/// Zero-sized conversion type.
pub struct ErrorMapper;
