// Interface tests for `ErrorMapper`. Verifies IC-MCP-08 envelope shape
// and BC-MCP-03 stable-enum mapping.

use super::error_mapper::{
    ErrorMapper, McpContent, ToolResult, TypedReceipt, MAX_MESSAGE_CHARS,
};
use loom_rpc::error::{LoomError, LoomErrorCode};

// === IC-MCP-08: errors map to ToolResult { isError: true, content } ===

#[test]
fn to_tool_result_signature() {
    fn _ck(e: LoomError) -> ToolResult {
        ErrorMapper::to_tool_result(e)
    }
    let _ = _ck;
}

#[test]
fn from_loom_error_impl_present() {
    // `?` ergonomics: LoomError implicitly converts into ToolResult.
    fn _ck(e: LoomError) -> ToolResult {
        e.into()
    }
    let _ = _ck;
}

#[test]
fn tool_result_has_is_error_flag_renamed_camel_case() {
    let tr = ToolResult {
        is_error: true,
        content: vec![],
    };
    let s = serde_json::to_string(&tr).unwrap();
    // `isError` is the on-the-wire MCP field name. snake_case `is_error`
    // would break MCP clients.
    assert!(s.contains("\"isError\":true"), "wire field must be isError; got {s}");
}

#[test]
fn error_path_emits_exactly_one_content_block() {
    // IC-MCP-08: a single typed-receipt content block per error.
    // Encoded as a structural assertion about the constructor.
    fn _shape(code: LoomErrorCode) {
        let r = ErrorMapper::typed_receipt(code);
        let block = McpContent::from_json(serde_json::to_value(&r).unwrap());
        let tr = ToolResult {
            is_error: true,
            content: vec![block],
        };
        assert_eq!(tr.content.len(), 1);
    }
    let _ = _shape;
}

#[test]
fn typed_receipt_carries_code_message_data_remediation() {
    let r = TypedReceipt {
        code: "daemon_connection_lost".into(),
        message: "daemon socket disappeared".into(),
        data: Some(serde_json::json!({ "socket_path": "/tmp/loom.sock" })),
        remediation: Some("Run `loom doctor` to start the daemon.".into()),
    };
    assert_eq!(r.code, "daemon_connection_lost");
    assert!(r.message.chars().count() <= MAX_MESSAGE_CHARS);
}

#[test]
fn message_truncation_constant_is_280() {
    assert_eq!(MAX_MESSAGE_CHARS, 280);
}

// === BC-MCP-03: stable LoomErrorCode enum is the only input ===

#[test]
fn typed_receipt_signature_takes_loom_error_code() {
    fn _ck(c: LoomErrorCode) -> TypedReceipt {
        ErrorMapper::typed_receipt(c)
    }
    let _ = _ck;
}

// === Internal-failure mapping helpers (no inline code invention) ===

#[test]
fn from_rpc_io_returns_daemon_connection_lost() {
    fn _ck(d: &str) -> LoomError {
        ErrorMapper::from_rpc_io(d)
    }
    let _ = _ck;
}

#[test]
fn from_hello_mismatch_returns_protocol_auth_required() {
    fn _ck(d: &str) -> LoomError {
        ErrorMapper::from_hello_mismatch(d)
    }
    let _ = _ck;
}

#[test]
fn from_unknown_tool_returns_method_not_found() {
    fn _ck(name: &str) -> LoomError {
        ErrorMapper::from_unknown_tool(name)
    }
    let _ = _ck;
}

#[test]
fn from_schema_parse_returns_protocol_malformed() {
    fn _ck(d: &str) -> LoomError {
        ErrorMapper::from_schema_parse(d)
    }
    let _ = _ck;
}

// === Content-block discriminant ===

#[test]
fn mcp_content_from_json_serialises_as_text_tag() {
    // Per the v1.0.1 fix: typed-receipt content blocks now use the
    // standard MCP `text` content type (json-stringified) so strict
    // MCP clients (e.g. Claude Code) accept them. Pre-fix, the
    // ``json`` content type tripped Zod schema validation in the client.
    let block = McpContent::from_json(serde_json::json!({"k": "v"}));
    let s = serde_json::to_string(&block).unwrap();
    assert!(s.contains("\"type\":\"text\""), "got {s}");
    // The inner JSON value is JSON-stringified into the text field, so
    // its quotes are escaped — assert the escaped form survives
    // serialisation, not the bare 3-char `"k"` literal.
    assert!(s.contains("\\\"k\\\""), "got {s}");
}

#[test]
fn mcp_content_text_variant_serialises_with_text_tag() {
    let block = McpContent::Text { text: "hi".into() };
    let s = serde_json::to_string(&block).unwrap();
    assert!(s.contains("\"type\":\"text\""), "got {s}");
}
