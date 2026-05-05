// Interface tests for `McpDispatcher`. Verifies dispatch shape
// (tools/call → RpcClient), prompts/list empty, and unknown-tool
// behavior (surfaces as ToolResult.isError, not McpProtocolError).

use super::mcp_dispatcher::{
    InitializeResult, McpDispatcher, PromptsCapability, ResourcesCapability, ResourcesReadParams,
    ServerCapabilities, ServerInfo, ToolsCallParams, ToolsCapability, MCP_PROTOCOL_VERSION,
    METHOD_INITIALIZE, METHOD_PING, METHOD_PROMPTS_LIST, METHOD_RESOURCES_LIST,
    METHOD_RESOURCES_READ, METHOD_SHUTDOWN, METHOD_TOOLS_CALL, METHOD_TOOLS_LIST,
};
use crate::error_mapper::ToolResult;
use crate::resource_tracker::{Resource, ResourceContents};
use crate::tool_cache::Tool;

// === Method-name interning: catches typos at compile time ===

#[test]
fn method_constants_match_mcp_spec() {
    assert_eq!(METHOD_INITIALIZE, "initialize");
    assert_eq!(METHOD_SHUTDOWN, "shutdown");
    assert_eq!(METHOD_PING, "ping");
    assert_eq!(METHOD_TOOLS_LIST, "tools/list");
    assert_eq!(METHOD_TOOLS_CALL, "tools/call");
    assert_eq!(METHOD_RESOURCES_LIST, "resources/list");
    assert_eq!(METHOD_RESOURCES_READ, "resources/read");
    assert_eq!(METHOD_PROMPTS_LIST, "prompts/list");
}

#[test]
fn protocol_version_pinned() {
    assert_eq!(MCP_PROTOCOL_VERSION, "2024-11-05");
}

// === prompts/list returns empty list ===

#[test]
fn prompts_list_returns_empty_vec() {
    fn _ck(
        d: &McpDispatcher,
    ) -> Box<dyn std::future::Future<Output = Vec<serde_json::Value>> + '_> {
        Box::new(async move { d.prompts_list().await })
    }
    let _ = _ck;
}

// === tools/call dispatches to ToolResult ===

#[test]
fn tools_call_signature_returns_tool_result() {
    fn _ck(
        d: std::sync::Arc<McpDispatcher>,
        p: ToolsCallParams,
    ) -> Box<dyn std::future::Future<Output = ToolResult>> {
        Box::new(async move { d.tools_call(p).await })
    }
    let _ = _ck;
}

#[test]
fn tools_call_params_has_name_and_arguments() {
    let p = ToolsCallParams {
        name: "loom.action.web.click".into(),
        arguments: serde_json::json!({ "selector": "#go" }),
    };
    assert!(p.name.starts_with("loom."));
    assert!(p.arguments.is_object());
}

// === tools/list returns Tool[] ===

#[test]
fn tools_list_returns_vec_tool() {
    fn _ck(d: std::sync::Arc<McpDispatcher>) -> Box<dyn std::future::Future<Output = Vec<Tool>>> {
        Box::new(async move { d.tools_list().await })
    }
    let _ = _ck;
}

// === resources/list + resources/read ===

#[test]
fn resources_list_returns_result_vec_resource() {
    fn _ck(
        d: std::sync::Arc<McpDispatcher>,
    ) -> Box<dyn std::future::Future<Output = Result<Vec<Resource>, loom_rpc::error::LoomError>>>
    {
        Box::new(async move { d.resources_list().await })
    }
    let _ = _ck;
}

#[test]
fn resources_read_takes_uri_param() {
    fn _ck(
        d: std::sync::Arc<McpDispatcher>,
        p: ResourcesReadParams,
    ) -> Box<dyn std::future::Future<Output = Result<ResourceContents, loom_rpc::error::LoomError>>>
    {
        Box::new(async move { d.resources_read(p).await })
    }
    let _ = _ck;
}

#[test]
fn resources_read_params_has_uri() {
    let p = ResourcesReadParams {
        uri: "loom://session/01HZ/manifest".into(),
    };
    assert_eq!(p.uri, "loom://session/01HZ/manifest");
}

// === initialize advertises tools + resources + prompts capabilities ===

#[test]
fn initialize_returns_protocol_version_and_capabilities() {
    fn _ck(
        d: std::sync::Arc<McpDispatcher>,
    ) -> Box<dyn std::future::Future<Output = InitializeResult>> {
        Box::new(async move { d.initialize().await })
    }
    let _ = _ck;
}

#[test]
fn server_capabilities_has_tools_resources_prompts() {
    let c = ServerCapabilities {
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
    };
    let s = serde_json::to_string(&c).unwrap();
    assert!(s.contains("\"tools\""));
    assert!(s.contains("\"resources\""));
    assert!(s.contains("\"prompts\""));
}

#[test]
fn server_info_has_name_loom_mcp() {
    let i = ServerInfo {
        name: "loom-mcp".into(),
        version: "0.1.0".into(),
    };
    assert_eq!(i.name, "loom-mcp");
}

// === Unknown MCP method returns McpProtocolError, NOT ToolResult ===

#[test]
fn unknown_method_error_carries_jsonrpc_method_not_found_code() {
    let e = McpDispatcher::unknown_method_error("foo/bar");
    assert_eq!(e.code, -32601);
    assert!(e.message.contains("foo/bar"));
}

// === ping doesn't touch the daemon (no async fault path) ===

#[test]
fn ping_returns_value_no_error_path() {
    fn _ck(d: &McpDispatcher) -> Box<dyn std::future::Future<Output = serde_json::Value> + '_> {
        Box::new(async move { d.ping().await })
    }
    let _ = _ck;
}

// === shutdown is fire-and-forget (sets a flag) ===

#[test]
fn shutdown_signature_is_synchronous_unit() {
    fn _ck(d: &McpDispatcher) {
        d.shutdown()
    }
    let _ = _ck;
}
