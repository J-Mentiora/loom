// McpDispatcher — routes MCP methods to handlers.
// Types only. Implementation in mod.rs.

use crate::mcp_observability::McpObservability;
use crate::resource_tracker::ResourceTracker;
use crate::rpc_client::RpcClient;
use crate::tool_cache::ToolCache;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Server capabilities advertised in `initialize`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerCapabilities {
    pub tools: ToolsCapability,
    pub resources: ResourcesCapability,
    pub prompts: PromptsCapability,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolsCapability {
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourcesCapability {
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
    pub subscribe: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptsCapability {
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

/// Result body of `initialize`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

/// Params of `tools/call`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsCallParams {
    pub name: String,
    #[serde(default)]
    pub arguments: serde_json::Value,
}

/// Params of `resources/read`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourcesReadParams {
    pub uri: String,
}

pub const METHOD_INITIALIZE: &str = "initialize";
pub const METHOD_SHUTDOWN: &str = "shutdown";
pub const METHOD_PING: &str = "ping";
pub const METHOD_TOOLS_LIST: &str = "tools/list";
pub const METHOD_TOOLS_CALL: &str = "tools/call";
pub const METHOD_RESOURCES_LIST: &str = "resources/list";
pub const METHOD_RESOURCES_READ: &str = "resources/read";
pub const METHOD_PROMPTS_LIST: &str = "prompts/list";
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Concrete dispatcher.
pub struct McpDispatcher {
    pub(crate) tool_cache: Arc<ToolCache>,
    pub(crate) resource_tracker: Arc<ResourceTracker>,
    pub(crate) rpc: Arc<RpcClient>,
    #[allow(dead_code)]
    pub(crate) obs: Arc<McpObservability>,
    pub(crate) shutdown_flag: Arc<std::sync::atomic::AtomicBool>,
    /// Implicit session id auto-created on first tool call and reused
    /// across the MCP server's lifetime. Lets MCP clients call
    /// `loom.web.*` tools without first having to call session.create
    /// (which isn't even in the tool list because `rpc.schemas` only
    /// returns schema-driven methods, not the BUILTIN_CORE_METHODS
    /// that session.* belongs to). Closed on shutdown.
    pub(crate) implicit_session: Arc<tokio::sync::Mutex<Option<String>>>,
}
