//! `mcp_adapter` — re-exports the implementation submodule.
pub mod mcp_adapter;
pub use mcp_adapter::*;

#[cfg(test)]
mod interface_tests;

use crate::request_router::request_router::RequestRouterApi;
use crate::schema_provider::schema_provider::SchemaProviderApi;
use std::sync::Arc;

impl McpAdapter {
    pub fn new(
        router: Arc<dyn RequestRouterApi>,
        schemas: Arc<dyn SchemaProviderApi>,
    ) -> Arc<Self> {
        Arc::new(Self { router, schemas })
    }

    pub fn list_tools(&self) -> Vec<McpTool> {
        self.schemas
            .get_registry_snapshot()
            .methods
            .into_iter()
            .map(|m| McpTool {
                name: m.method.clone(),
                description: format!("RPC method {}", m.method),
                input_schema: m.request,
            })
            .collect()
    }

    pub async fn handle_tool_call(&self, call: McpToolCall) -> Result<McpToolResult, McpError> {
        let known = self.schemas.registered_methods();
        if !known.contains(&call.name) {
            return Err(McpError::UnknownTool { name: call.name });
        }
        let bytes = self.router.dispatch(&call.name, call.arguments).await;
        let json: serde_json::Value =
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);

        if json.get("code").is_some() {
            let err: crate::error_translator::error_translator::JsonRpcError =
                serde_json::from_value(json.clone()).map_err(|_| {
                    McpError::Rpc(crate::error_translator::error_translator::JsonRpcError {
                        code:
                            crate::error_translator::error_translator::LoomErrorCode::InternalError,
                        message: "failed to decode error envelope".to_string(),
                        data: None,
                    })
                })?;
            return Err(McpError::Rpc(err));
        }

        Ok(McpToolResult {
            content: vec![McpContent::Text {
                text: String::from_utf8_lossy(&bytes).to_string(),
            }],
            is_error: false,
        })
    }

    pub fn assert_tool_list_parity(&self) -> Result<(), ParityViolation> {
        let rpc_methods: std::collections::HashSet<String> =
            self.schemas.registered_methods().into_iter().collect();
        let mcp_tools: std::collections::HashSet<String> =
            self.list_tools().into_iter().map(|t| t.name).collect();

        let missing_in_mcp: Vec<String> = rpc_methods.difference(&mcp_tools).cloned().collect();
        let missing_in_rpc: Vec<String> = mcp_tools.difference(&rpc_methods).cloned().collect();

        if missing_in_mcp.is_empty() && missing_in_rpc.is_empty() {
            Ok(())
        } else {
            Err(ParityViolation {
                missing_in_mcp,
                missing_in_rpc,
            })
        }
    }
}
