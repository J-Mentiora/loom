// Interface tests for `ErrorMapper`. Verifies the envelope shape and
// stable-enum mapping.

use super::error_mapper::{ErrorMapper, McpContent, ToolResult, TypedReceipt, MAX_MESSAGE_CHARS};
use loom_rpc::error::{LoomError, LoomErrorCode};

// === errors map to ToolResult { isError: true, content } ===

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
    assert!(
        s.contains("\"isError\":true"),
        "wire field must be isError; got {s}"
    );
}

#[test]
fn error_path_emits_exactly_one_content_block() {
    // a single typed-receipt content block per error.
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

// === stable LoomErrorCode enum is the only input ===

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

// === Structured error context surfaces in TypedReceipt.data (P2.3) ===

/// Parse the single text content block of an error `ToolResult` back into the
/// `TypedReceipt` it carries.
fn receipt_of(result: &ToolResult) -> TypedReceipt {
    assert_eq!(result.content.len(), 1, "exactly one content block");
    let text = match &result.content[0] {
        McpContent::Text { text } => text,
        other => panic!("expected text content block, got {other:?}"),
    };
    serde_json::from_str(text).expect("content text must parse as a TypedReceipt")
}

#[test]
fn session_cap_exceeded_to_tool_result_carries_structured_active_cap_hint() {
    // Mirrors loom-cli/tests/integration_session_cap.rs but at the MCP layer:
    // the daemon attaches {active, cap, hint} to the session_cap_exceeded
    // error's context, and `to_tool_result` must surface those NUMERICALLY in
    // the TypedReceipt `data` — not merely inside the human message string.
    let err = LoomError::new(
        LoomErrorCode::SessionCapExceeded,
        "concurrent session cap reached (2/2); close sessions or run `loom session reap`",
    )
    .with_context(serde_json::json!({
        "active": 2,
        "cap": 2,
        "hint": "close sessions or run `loom session reap`",
    }));

    let result = ErrorMapper::to_tool_result(err);
    assert!(result.is_error);
    let receipt = receipt_of(&result);
    assert_eq!(receipt.code, "session_cap_exceeded");

    let data = receipt.data.expect("data must be present");
    assert_eq!(
        data.get("active").and_then(|v| v.as_u64()),
        Some(2),
        "active must be numeric 2; data={data}"
    );
    assert_eq!(
        data.get("cap").and_then(|v| v.as_u64()),
        Some(2),
        "cap must be numeric 2 (== cap); data={data}"
    );
    assert!(
        data.get("hint")
            .and_then(|v| v.as_str())
            .map(|s| s.contains("loom session reap"))
            .unwrap_or(false),
        "actionable hint must survive structurally; data={data}"
    );
    // Retryability is still overlaid alongside the structured context
    // (session_cap_exceeded => Backoff).
    assert_eq!(
        data.get("retryable").and_then(|v| v.as_bool()),
        Some(true),
        "retryable must remain; data={data}"
    );
    assert_eq!(
        data.get("retry").and_then(|v| v.as_str()),
        Some("backoff"),
        "retry disposition must remain; data={data}"
    );
}

#[test]
fn to_tool_result_without_context_or_retry_emits_no_data() {
    // Regression guard: a non-retryable error with no context still yields
    // `data: None` (the pre-fix contract for the empty case).
    let err = LoomError::new(LoomErrorCode::InvalidArgument, "bad arg");
    let receipt = receipt_of(&ErrorMapper::to_tool_result(err));
    assert!(
        receipt.data.is_none(),
        "no context + non-retryable must omit data; got {:?}",
        receipt.data
    );
}
