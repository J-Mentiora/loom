pub mod tool_cache;
pub use tool_cache::*;

#[cfg(test)]
mod interface_tests;

use crate::error_mapper::ErrorMapper;
use loom_rpc::error::LoomError;
use std::sync::Arc;

// Wire types from rpc.schemas RPC response (field names differ from local RpcMethodSchema).
#[derive(serde::Deserialize)]
struct WireMethod {
    method: String,
    request: serde_json::Value,
    response: serde_json::Value,
}

#[derive(serde::Deserialize)]
struct WireRegistry {
    methods: Vec<WireMethod>,
    #[allow(dead_code)]
    source_wit_sha256: Option<String>,
}

impl ToolCache {
    pub fn new(rpc: Arc<crate::rpc_client::RpcClient>) -> Arc<Self> {
        Arc::new(Self {
            tools: Arc::new(tokio::sync::RwLock::new(vec![])),
            rpc,
        })
    }

    pub async fn prime(self: &Arc<Self>) -> Result<(), LoomError> {
        let raw = self.rpc.call("rpc.schemas", serde_json::json!({})).await?;
        let registry: WireRegistry = serde_json::from_value(raw)
            .map_err(|e| ErrorMapper::from_schema_parse(&e.to_string()))?;
        let mut built = Vec::new();
        for m in registry.methods {
            let schema = RpcMethodSchema {
                name: m.method,
                request_schema: m.request,
                response_schema: m.response,
                doc_comment: String::new(),
            };
            match Self::tool_from_method(&schema) {
                Ok(tool) => built.push(tool),
                Err(e) => tracing::error!(error = %e, name = %schema.name, "skipping malformed rpc method"),
            }
        }
        *self.tools.write().await = built;
        Ok(())
    }

    pub async fn refresh(self: &Arc<Self>) -> Result<(), LoomError> {
        self.prime().await
    }

    pub async fn invalidate(&self) {
        *self.tools.write().await = vec![];
    }

    pub async fn list(&self) -> Vec<Tool> {
        self.tools.read().await.clone()
    }

    pub async fn get(&self, mcp_name: &str) -> Option<Tool> {
        self.tools
            .read()
            .await
            .iter()
            .find(|t| t.name == mcp_name)
            .cloned()
    }

    pub fn tool_from_method(method: &RpcMethodSchema) -> Result<Tool, LoomError> {
        if !Self::is_snake_case_dotted(&method.name) {
            return Err(ErrorMapper::from_schema_parse(&format!(
                "invalid rpc method name: {}",
                method.name
            )));
        }
        let mut input_schema = Self::canonicalise(&method.request_schema)?;
        // The MCP dispatcher auto-injects `session` from its implicit
        // session for any web.* call, so strip the field from the
        // tool's advertised input_schema. Otherwise a strict MCP
        // client (Claude Desktop's tool-call validator) refuses to
        // call the tool because its `required: ["session", ...]` says
        // session is mandatory but the client has no way to obtain a
        // session_id (session.create isn't exposed as a tool — see
        // McpDispatcher::ensure_implicit_session).
        if method.name.starts_with("web.") {
            Self::drop_session_from_schema(&mut input_schema);
        }
        Ok(Tool {
            name: Self::rpc_to_mcp_name(&method.name),
            description: method.doc_comment.clone(),
            input_schema,
        })
    }

    /// Remove the `session` property and its `required` entry from a
    /// JSON-schema describing a tool's input. The MCP dispatcher auto-
    /// injects the implicit session id, so the field is invisible to
    /// the MCP client.
    fn drop_session_from_schema(schema: &mut serde_json::Value) {
        let Some(obj) = schema.as_object_mut() else {
            return;
        };
        if let Some(props) = obj.get_mut("properties").and_then(|v| v.as_object_mut()) {
            props.remove("session");
        }
        if let Some(req) = obj.get_mut("required").and_then(|v| v.as_array_mut()) {
            req.retain(|v| v.as_str() != Some("session"));
        }
    }

    pub fn rpc_to_mcp_name(rpc_name: &str) -> String {
        format!("{TOOL_NAME_PREFIX}{rpc_name}")
    }

    pub fn mcp_to_rpc_name(mcp_name: &str) -> Option<&str> {
        mcp_name.strip_prefix(TOOL_NAME_PREFIX)
    }

    pub fn is_snake_case_dotted(name: &str) -> bool {
        if name.is_empty() {
            return false;
        }
        name.split('.').all(|seg| {
            !seg.is_empty()
                && seg
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
    }

    pub fn canonicalise(value: &serde_json::Value) -> Result<serde_json::Value, LoomError> {
        let s = serde_jcs::to_string(value)
            .map_err(|e| ErrorMapper::from_schema_parse(&e.to_string()))?;
        serde_json::from_str(&s).map_err(|e| ErrorMapper::from_schema_parse(&e.to_string()))
    }
}
