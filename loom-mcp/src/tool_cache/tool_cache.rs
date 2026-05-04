// ToolCache — Vec<Tool> built from rpc.schemas, refreshed on reconnect.
// Types only. Implementation in mod.rs.

use crate::rpc_client::RpcClient;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// MCP Tool shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

/// Method record used by ToolCache (local mirror of loom-rpc MethodSchema).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcMethodSchema {
    pub name: String,
    pub request_schema: serde_json::Value,
    pub response_schema: serde_json::Value,
    pub doc_comment: String,
}

/// Wire shape of `rpc.schemas` response — local type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaRegistry {
    pub methods: Vec<RpcMethodSchema>,
}

/// Concrete cache.
pub struct ToolCache {
    pub(crate) tools: Arc<tokio::sync::RwLock<Vec<Tool>>>,
    pub(crate) rpc: Arc<RpcClient>,
}

/// MCP tool-name prefix.
pub const TOOL_NAME_PREFIX: &str = "loom.";
