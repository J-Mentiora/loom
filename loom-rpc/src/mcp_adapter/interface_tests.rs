// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-rpc/modules/mcp_adapter/interface_tests.rs` instead.
// Interface tests for `McpAdapter`. Verifies UX-13 / AC-PROTO-03.1
// 1:1 tool↔method mapping, ToolResult shape, no-bypass routing
// through RequestRouter.

use super::mcp_adapter::{
    McpAdapter, McpContent, McpError, McpTool, McpToolCall, McpToolResult, ParityViolation,
};
use crate::error_translator::error_translator::JsonRpcError;
use crate::request_router::request_router::RequestRouterApi;
use crate::schema_provider::schema_provider::SchemaProviderApi;
use std::sync::Arc;

#[test]
fn constructor_takes_router_and_schemas_for_bit_equal_tool_list() {
    // UX-13: McpAdapter reuses the RpcModule so MCP tools and JSON-RPC
    // methods are bit-equal.
    fn _ck(r: Arc<dyn RequestRouterApi>, s: Arc<dyn SchemaProviderApi>) -> Arc<McpAdapter> {
        McpAdapter::new(r, s)
    }
    let _ = _ck;
}

#[test]
fn mcp_tool_carries_name_description_input_schema() {
    fn _ck(t: &McpTool) {
        let _: &String = &t.name;
        let _: &String = &t.description;
        let _: &serde_json::Value = &t.input_schema;
    }
    let _ = _ck;
}

#[test]
fn mcp_tool_call_carries_name_and_arguments() {
    fn _ck(c: &McpToolCall) {
        let _: &String = &c.name;
        let _: &serde_json::Value = &c.arguments;
    }
    let _ = _ck;
}

#[test]
fn mcp_tool_result_carries_content_array_and_is_error_flag() {
    fn _ck(r: &McpToolResult) {
        let _: &Vec<McpContent> = &r.content;
        let _: &bool = &r.is_error;
    }
    let _ = _ck;
}

#[test]
fn mcp_content_supports_text_variant() {
    let c = McpContent::Text { text: "{}".into() };
    let s = serde_json::to_string(&c).unwrap();
    assert!(s.contains("\"type\":\"text\""));
}

#[test]
fn mcp_error_distinguishes_malformed_unknown_tool_rpc() {
    let _ = McpError::Malformed;
    let _ = McpError::UnknownTool { name: "x".into() };
    fn _wrap(e: JsonRpcError) -> McpError {
        McpError::Rpc(e)
    }
    let _ = _wrap;
}

#[test]
fn list_tools_signature_returns_vec_mcp_tool() {
    fn _ck(a: &McpAdapter) -> Vec<McpTool> {
        a.list_tools()
    }
    let _ = _ck;
}

#[test]
fn handle_tool_call_signature_async_returns_tool_result_or_error() {
    fn _ck() {
        async fn _go(a: &McpAdapter, c: McpToolCall) -> Result<McpToolResult, McpError> {
            a.handle_tool_call(c).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

#[test]
fn assert_tool_list_parity_catches_drift_at_startup() {
    // UX-13 / AC-PROTO-03.1: refuse to start if drift detected.
    fn _ck(a: &McpAdapter) -> Result<(), ParityViolation> {
        a.assert_tool_list_parity()
    }
    let _ = _ck;
    let _ = ParityViolation {
        missing_in_mcp: vec!["session.foo".into()],
        missing_in_rpc: vec![],
    };
}
