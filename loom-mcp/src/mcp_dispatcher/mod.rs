pub mod mcp_dispatcher;
pub use mcp_dispatcher::*;

#[cfg(test)]
mod interface_tests;

use crate::error_mapper::ErrorMapper;
use crate::stdio_transport::{McpProtocolError, McpRequest, McpResponse};
use loom_rpc::error::LoomError;
use std::sync::atomic::Ordering;
use std::sync::Arc;

impl McpDispatcher {
    pub fn new(
        tool_cache: Arc<crate::tool_cache::ToolCache>,
        resource_tracker: Arc<crate::resource_tracker::ResourceTracker>,
        rpc: Arc<crate::rpc_client::RpcClient>,
        obs: Arc<crate::mcp_observability::McpObservability>,
        shutdown_flag: Arc<std::sync::atomic::AtomicBool>,
    ) -> Arc<Self> {
        Arc::new(Self {
            tool_cache,
            resource_tracker,
            rpc,
            obs,
            shutdown_flag,
            implicit_session: Arc::new(tokio::sync::Mutex::new(None)),
        })
    }

    /// Look up or lazily create the implicit session that backs every
    /// `loom.web.*` tool call. Mutex'd so concurrent first-tool-call
    /// races serialize to a single session.create (one session per MCP
    /// server process, reused across the conversation).
    async fn ensure_implicit_session(self: &Arc<Self>) -> Result<String, LoomError> {
        let mut guard = self.implicit_session.lock().await;
        if let Some(id) = guard.as_ref() {
            return Ok(id.clone());
        }
        // Default to standard profile so denylist surprises don't
        // confuse MCP clients (the safe-profile evaluate denylist would
        // bounce window.location and friends; that's a CLI safety
        // preset, not an MCP-default expectation).
        let resp = self
            .rpc
            .call(
                "session.create",
                serde_json::json!({ "profile": "standard" }),
            )
            .await?;
        let id = resp
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                crate::error_mapper::ErrorMapper::from_schema_parse(
                    "session.create response missing session_id",
                )
            })?
            .to_string();
        *guard = Some(id.clone());
        Ok(id)
    }

    /// Close the implicit session if one was created. Best-effort; an
    /// error here means the daemon already cleaned the session up
    /// (e.g. operator hit `loom session close` out-of-band) and there's
    /// nothing to do.
    pub async fn close_implicit_session(self: &Arc<Self>) {
        let mut guard = self.implicit_session.lock().await;
        if let Some(id) = guard.take() {
            let _ = self
                .rpc
                .call("session.close", serde_json::json!({ "session_id": id }))
                .await;
        }
    }

    pub async fn dispatch(self: &Arc<Self>, req: McpRequest) -> Option<McpResponse> {
        // Notifications (no id) → no response.
        let id = req.id.clone()?;

        // Route to per-method handlers.
        let (result_val, error_val) = match req.method.as_str() {
            METHOD_INITIALIZE => {
                let v = serde_json::to_value(self.initialize().await)
                    .unwrap_or(serde_json::Value::Null);
                (Some(v), None)
            }
            METHOD_PING => (Some(self.ping().await), None),
            METHOD_TOOLS_LIST => {
                let tools = self.tools_list().await;
                let v = serde_json::json!({ "tools": tools });
                (Some(v), None)
            }
            METHOD_TOOLS_CALL => {
                let params: ToolsCallParams =
                    serde_json::from_value(req.params).unwrap_or(ToolsCallParams {
                        name: String::new(),
                        arguments: serde_json::Value::Null,
                    });
                let tr = self.tools_call(params).await;
                let v = serde_json::to_value(tr).unwrap_or(serde_json::Value::Null);
                (Some(v), None)
            }
            METHOD_RESOURCES_LIST => match self.resources_list().await {
                Ok(resources) => {
                    let v = serde_json::json!({ "resources": resources });
                    (Some(v), None)
                }
                Err(e) => {
                    let tr = ErrorMapper::to_tool_result(e);
                    let v = serde_json::to_value(tr).unwrap_or(serde_json::Value::Null);
                    (Some(v), None)
                }
            },
            METHOD_RESOURCES_READ => {
                let params: ResourcesReadParams = serde_json::from_value(req.params)
                    .unwrap_or(ResourcesReadParams { uri: String::new() });
                match self.resources_read(params).await {
                    Ok(contents) => {
                        // MCP 2024-11-05 defines the resources/read result as
                        // { "contents": [TextResourceContents | BlobResourceContents] }
                        // — strict clients reject a bare contents object.
                        let c = serde_json::to_value(contents).unwrap_or(serde_json::Value::Null);
                        let v = serde_json::json!({ "contents": [c] });
                        (Some(v), None)
                    }
                    Err(e) => {
                        let tr = ErrorMapper::to_tool_result(e);
                        let v = serde_json::to_value(tr).unwrap_or(serde_json::Value::Null);
                        (Some(v), None)
                    }
                }
            }
            METHOD_PROMPTS_LIST => {
                let v = serde_json::json!({ "prompts": self.prompts_list().await });
                (Some(v), None)
            }
            METHOD_SHUTDOWN => {
                self.shutdown();
                (Some(serde_json::json!({})), None)
            }
            _ => {
                let err = Self::unknown_method_error(&req.method);
                return Some(McpResponse {
                    jsonrpc: "2.0".into(),
                    id,
                    result: None,
                    error: Some(err),
                });
            }
        };

        Some(McpResponse {
            jsonrpc: "2.0".into(),
            id,
            result: result_val,
            error: error_val,
        })
    }

    pub async fn initialize(self: &Arc<Self>) -> InitializeResult {
        InitializeResult {
            protocol_version: MCP_PROTOCOL_VERSION.into(),
            capabilities: ServerCapabilities {
                tools: ToolsCapability {
                    list_changed: false,
                },
                resources: ResourcesCapability {
                    list_changed: false,
                    subscribe: false,
                },
                prompts: PromptsCapability {
                    list_changed: false,
                },
            },
            server_info: ServerInfo {
                name: "loom-mcp".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
        }
    }

    pub fn shutdown(&self) {
        self.shutdown_flag.store(true, Ordering::SeqCst);
    }

    pub async fn ping(&self) -> serde_json::Value {
        serde_json::json!({})
    }

    pub async fn tools_list(self: &Arc<Self>) -> Vec<crate::tool_cache::Tool> {
        self.tool_cache.list().await
    }

    /// prime the tool cache from `rpc.schemas`.
    /// Idempotent — repeated calls re-fetch and overwrite. Called once
    /// at server startup by `mcp_main::run`; safe to retry on transient
    /// daemon-down.
    pub async fn prime_tool_cache(self: &Arc<Self>) -> Result<(), LoomError> {
        self.tool_cache.prime().await
    }

    pub async fn tools_call(
        self: &Arc<Self>,
        p: ToolsCallParams,
    ) -> crate::error_mapper::ToolResult {
        let rpc_method = match crate::tool_cache::ToolCache::mcp_to_rpc_name(&p.name) {
            None => {
                return ErrorMapper::to_tool_result(ErrorMapper::from_unknown_tool(&p.name));
            }
            Some(m) => m.to_string(),
        };
        // Auto-inject the implicit session for tools that need one.
        // The web.* family + their aliases all carry a `session` field;
        // anything else (rpc.schemas, future session-less endpoints) is
        // forwarded verbatim. Caller-supplied `session` takes
        // precedence so power-users can pin a specific session.
        let arguments = if rpc_method.starts_with("web.") {
            let mut args = match p.arguments {
                serde_json::Value::Object(m) => m,
                _ => serde_json::Map::new(),
            };
            if !args.contains_key("session") {
                match self.ensure_implicit_session().await {
                    Ok(id) => {
                        args.insert("session".to_string(), serde_json::Value::String(id));
                    }
                    Err(e) => return ErrorMapper::to_tool_result(e),
                }
            }
            serde_json::Value::Object(args)
        } else {
            p.arguments
        };
        self.rpc.call_as_tool_result(&rpc_method, arguments).await
    }

    pub async fn resources_list(
        self: &Arc<Self>,
    ) -> Result<Vec<crate::resource_tracker::Resource>, LoomError> {
        self.resource_tracker.list().await
    }

    pub async fn resources_read(
        self: &Arc<Self>,
        p: ResourcesReadParams,
    ) -> Result<crate::resource_tracker::ResourceContents, LoomError> {
        self.resource_tracker.read(&p.uri).await
    }

    pub async fn prompts_list(&self) -> Vec<serde_json::Value> {
        Vec::new()
    }

    pub fn unknown_method_error(method: &str) -> McpProtocolError {
        McpProtocolError {
            code: crate::stdio_transport::ERROR_METHOD_NOT_FOUND,
            message: format!("unknown MCP method: {method}"),
            data: None,
        }
    }
}
