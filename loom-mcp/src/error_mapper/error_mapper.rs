// ErrorMapper — single conversion point from `LoomError` to MCP
// `ToolResult { isError: true, content }`.
//
// Types only. Implementation in mod.rs.

use serde::{Deserialize, Serialize};

/// MCP content block carried inside a `ToolResult`.
///
/// Only the standard MCP content types (`text`, `image`, `resource`) are
/// portable across MCP clients. v1.0.0 emitted ``{"type": "json", "json":
/// ...}`` which trips strict MCP-client validators (e.g. Claude Code's
/// Zod schema rejects ``content[0]`` as a discriminated-union mismatch).
/// Json payloads are now stringified into a `text` block at construction
/// time; consumers who want structured access parse the text back as JSON.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpContent {
    Text {
        text: String,
    },
    /// Standard MCP `image` content block: base64-encoded bytes + MIME type.
    /// Emitted alongside the text receipt for screenshot-producing verbs so an
    /// MCP client receives a renderable image inline (the text block still
    /// carries `screenshot_after_hash` for wire-contract parity).
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
}

impl McpContent {
    /// Serialize a JSON value into a text content block.
    ///
    /// Used by both the success path (``rpc_client::call_as_tool_result``)
    /// and the error path (``ErrorMapper::to_tool_result``) so that all
    /// MCP responses use the standard `text` content type.
    pub fn from_json(value: serde_json::Value) -> Self {
        let text = serde_json::to_string(&value).unwrap_or_else(|_| String::from("null"));
        McpContent::Text { text }
    }
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
