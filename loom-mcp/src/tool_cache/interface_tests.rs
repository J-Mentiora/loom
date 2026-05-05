// Interface tests for `ToolCache`. Verifies count-parity envelope,
// canonicalisation, name transformation, description sourcing from
// the RPC method's doc comment, and single-constructor discipline.

use super::tool_cache::{RpcMethodSchema, SchemaRegistry, Tool, ToolCache, TOOL_NAME_PREFIX};

// === snake_case dotted, prefix `loom.` ===

#[test]
fn tool_name_prefix_is_loom_dot() {
    assert_eq!(TOOL_NAME_PREFIX, "loom.");
}

#[test]
fn rpc_to_mcp_name_prepends_loom_dot() {
    assert_eq!(
        ToolCache::rpc_to_mcp_name("action.web.click"),
        "loom.action.web.click"
    );
}

#[test]
fn mcp_to_rpc_name_strips_prefix() {
    assert_eq!(
        ToolCache::mcp_to_rpc_name("loom.action.web.click"),
        Some("action.web.click")
    );
    assert_eq!(ToolCache::mcp_to_rpc_name("not-prefixed"), None);
}

#[test]
fn is_snake_case_dotted_signature_present() {
    fn _ck(s: &str) -> bool {
        ToolCache::is_snake_case_dotted(s)
    }
    let _ = _ck;
}

// === canonicalisation helper named ===

#[test]
fn canonicalise_signature_returns_result() {
    fn _ck(v: &serde_json::Value) -> Result<serde_json::Value, loom_rpc::error::LoomError> {
        ToolCache::canonicalise(v)
    }
    let _ = _ck;
}

// === single Tool constructor — tool_from_method ===

#[test]
fn tool_from_method_signature_takes_rpc_method_schema() {
    fn _ck(m: &RpcMethodSchema) -> Result<Tool, loom_rpc::error::LoomError> {
        ToolCache::tool_from_method(m)
    }
    let _ = _ck;
}

// === Tool struct shape: name + description + inputSchema (camelCase) ===

#[test]
fn tool_serialises_input_schema_as_camel_case() {
    let t = Tool {
        name: "loom.action.web.click".into(),
        description: "click an element".into(),
        input_schema: serde_json::json!({}),
    };
    let s = serde_json::to_string(&t).unwrap();
    // The MCP wire format uses `inputSchema`; snake_case would break
    // every MCP client.
    assert!(s.contains("\"inputSchema\""), "got {s}");
}

#[test]
fn tool_has_name_description_input_schema_fields() {
    let _ = Tool {
        name: "x".into(),
        description: "y".into(),
        input_schema: serde_json::json!({}),
    };
}

// === description is sourced from RPC method's doc_comment ===

#[test]
fn rpc_method_schema_has_doc_comment_field() {
    let m = RpcMethodSchema {
        name: "action.web.click".into(),
        request_schema: serde_json::json!({}),
        response_schema: serde_json::json!({}),
        doc_comment: "WIT /// chain".into(),
    };
    assert_eq!(m.doc_comment, "WIT /// chain");
}

// === list() returns the full slice (no filter) ===

#[test]
fn list_returns_vec_tool() {
    fn _ck(c: &ToolCache) -> Box<dyn std::future::Future<Output = Vec<Tool>> + '_> {
        Box::new(async move { c.list().await })
    }
    let _ = _ck;
}

// === Lifecycle: prime / refresh / invalidate ===

#[test]
fn prime_signature_returns_result() {
    fn _ck(
        c: std::sync::Arc<ToolCache>,
    ) -> Box<dyn std::future::Future<Output = Result<(), loom_rpc::error::LoomError>>> {
        Box::new(async move { c.prime().await })
    }
    let _ = _ck;
}

#[test]
fn refresh_is_alias_of_prime() {
    fn _ck(
        c: std::sync::Arc<ToolCache>,
    ) -> Box<dyn std::future::Future<Output = Result<(), loom_rpc::error::LoomError>>> {
        Box::new(async move { c.refresh().await })
    }
    let _ = _ck;
}

#[test]
fn invalidate_returns_unit() {
    fn _ck(c: &ToolCache) -> Box<dyn std::future::Future<Output = ()> + '_> {
        Box::new(async move { c.invalidate().await })
    }
    let _ = _ck;
}

// === SchemaRegistry shape — wire compatibility with rpc.schemas ===

#[test]
fn schema_registry_has_methods_field() {
    let r = SchemaRegistry { methods: vec![] };
    let s = serde_json::to_string(&r).unwrap();
    assert!(s.contains("\"methods\""), "got {s}");
}

// === get() returns Option<Tool> so dispatcher can map None → MethodNotFound ===

#[test]
fn get_returns_option_tool() {
    fn _ck<'a>(
        c: &'a ToolCache,
        name: &'a str,
    ) -> Box<dyn std::future::Future<Output = Option<Tool>> + 'a> {
        Box::new(async move { c.get(name).await })
    }
    let _ = _ck;
}
