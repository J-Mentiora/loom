// Interface tests for `RpcClient`. Verifies the token-acquisition path,
// FSM shape, exp-backoff envelope, and single-reconnect-task discipline.

use super::rpc_client::{ConnectionState, JsonRpcCaller, OnConnected, RpcClient, RpcClientConfig};
use crate::mcp_observability::McpObservability;
use loom_rpc::error::LoomError;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

// === HELLO token from daemon auth artefact ===

#[test]
fn defaults_point_at_application_support_hello_token_on_macos() {
    let cfg = RpcClientConfig::defaults();
    let s = cfg.hello_token_path.display().to_string();
    // Default path must be the daemon's published auth artefact, NOT
    // loom-cli's token state file.
    if cfg!(target_os = "macos") {
        assert!(
            s.contains("loom/auth/hello.token"),
            "default macOS path must be ~/Library/Application Support/loom/auth/hello.token; got {s}"
        );
    }
}

#[test]
fn hello_token_path_is_overridable_via_config() {
    let mut cfg = RpcClientConfig::defaults();
    let custom = PathBuf::from("/tmp/custom-hello.token");
    cfg.hello_token_path = custom.clone();
    assert_eq!(cfg.hello_token_path, custom);
}

#[test]
fn read_hello_token_signature() {
    fn _ck(p: &std::path::Path) -> Result<String, LoomError> {
        RpcClient::read_hello_token(p)
    }
    let _ = _ck;
}

// === FSM shape ===

#[test]
fn connection_state_has_three_variants() {
    let _all = [
        ConnectionState::Connecting,
        ConnectionState::Connected,
        ConnectionState::Disconnected,
    ];
}

#[test]
fn state_method_returns_connection_state() {
    fn _ck(c: &RpcClient) -> Box<dyn std::future::Future<Output = ConnectionState> + '_> {
        Box::new(async move { c.state().await })
    }
    let _ = _ck;
}

// === Exp-backoff envelope: 100ms initial → 5s cap ===

#[test]
fn defaults_backoff_initial_is_100ms() {
    let cfg = RpcClientConfig::defaults();
    assert_eq!(cfg.backoff_initial, Duration::from_millis(100));
}

#[test]
fn defaults_backoff_cap_is_5s() {
    let cfg = RpcClientConfig::defaults();
    assert_eq!(cfg.backoff_cap, Duration::from_secs(5));
}

#[test]
fn next_backoff_caps_at_configured_max() {
    let cfg = RpcClientConfig::defaults();
    // After many failures, the delay must not exceed the cap.
    let d = RpcClient::next_backoff(&cfg, 30);
    assert!(
        d <= cfg.backoff_cap,
        "delay {d:?} must not exceed cap {:?}",
        cfg.backoff_cap
    );
}

// === Client-side call deadline sits above the daemon's request deadline ===

#[test]
fn defaults_call_timeout_exceeds_daemon_request_deadline() {
    let cfg = RpcClientConfig::defaults();
    // The daemon's server-side per-request deadline is
    // `LOOM_REQUEST_TIMEOUT_MS` (default 30 s). The client deadline must sit
    // above it so the daemon's typed `request_timeout` envelope wins the
    // race whenever the daemon is healthy enough to produce one.
    assert!(
        cfg.call_timeout > Duration::from_secs(30),
        "call_timeout {:?} must exceed the daemon's 30 s request deadline",
        cfg.call_timeout
    );
}

#[test]
fn next_backoff_starts_at_initial_for_first_failure() {
    let cfg = RpcClientConfig::defaults();
    let d = RpcClient::next_backoff(&cfg, 0);
    assert_eq!(d, cfg.backoff_initial);
}

// === socket path is daemon-owned, no extra spawns visible at API ===

#[test]
fn defaults_socket_path_is_loom_sock() {
    let cfg = RpcClientConfig::defaults();
    let s = cfg.socket_path.display().to_string();
    assert!(s.contains("loom.sock"), "got {s}");
}

#[test]
fn new_returns_arc_so_reconnect_task_can_share_state() {
    fn _ck(cfg: RpcClientConfig, obs: Arc<McpObservability>) -> Arc<RpcClient> {
        RpcClient::new(cfg, obs)
    }
    let _ = _ck;
}

// === call() signature returns LoomError so ErrorMapper can convert ===

#[test]
fn call_signature_returns_result_with_loom_error() {
    fn _ck(
        c: Arc<RpcClient>,
    ) -> Box<dyn std::future::Future<Output = Result<serde_json::Value, LoomError>>> {
        Box::new(async move {
            c.call("session.list", serde_json::json!({ "status": "active" }))
                .await
        })
    }
    let _ = _ck;
}

// === On-connected callback lets ToolCache hook prime/refresh ===

#[test]
fn register_on_connected_takes_arc_dyn_fn() {
    fn _ck(c: &RpcClient, cb: OnConnected) -> Box<dyn std::future::Future<Output = ()> + '_> {
        Box::new(async move { c.register_on_connected(cb).await })
    }
    let _ = _ck;
}

// === JsonRpcCaller trait is the substitution boundary ===

#[test]
fn json_rpc_caller_trait_present_for_test_substitution() {
    // Compile-time only: ensures the trait is importable and has the
    // expected raw_call shape.
    fn _ck(_c: &dyn JsonRpcCaller) {}
    let _ = _ck;
}

// === connect() is async and returns Result ===

#[test]
fn connect_returns_result_unit() {
    fn _ck(c: Arc<RpcClient>) -> Box<dyn std::future::Future<Output = Result<(), LoomError>>> {
        Box::new(async move { c.connect().await })
    }
    let _ = _ck;
}

// === Reproduce-first (mcp-screenshot-delivery): web.screenshot must
// deliver inline PNG image bytes to MCP clients. ===
//
// Today `call_as_tool_result` stringifies the receipt JSON into a single
// `text` block; no `image` content block is emitted, so an MCP client
// only sees `screenshot_after_hash` (a string) and can never render the
// screenshot. These tests assert the post-fix behaviour and therefore
// FAIL until the image-delivery path lands.

use crate::error_mapper::{McpContent, ToolResult};
use serde_json::json;

/// Canned JSON-RPC transport: answers `web.screenshot` with a receipt
/// carrying a known `screenshot_after_hash`, and `content.get` with a
/// hex-encoded blob whose first bytes are the PNG magic number. A
/// base64 of any PNG begins with the ASCII prefix `iVBORw0KGg`, which we
/// assert on without pulling a base64 decoder into the test.
struct ScreenshotFakeCaller;

// Hex of: 89 50 4E 47 0D 0A 1A 0A  00 00 00 0D 49 48 44 52  (PNG magic + IHDR start)
const FAKE_PNG_HEX: &str = "89504e470d0a1a0a0000000d49484452";
const FAKE_SHOT_HASH: &str = "1111111111111111111111111111111111111111111111111111111111111111";

#[async_trait::async_trait]
impl JsonRpcCaller for ScreenshotFakeCaller {
    async fn raw_call(
        &self,
        method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, LoomError> {
        match method {
            "web.screenshot" => Ok(json!({
                "ok": true,
                "screenshot_after_hash": FAKE_SHOT_HASH,
                "emitted_at_ms": 123u64
            })),
            "content.get" => Ok(json!({
                "artifact_ref": FAKE_SHOT_HASH,
                "data_hex": FAKE_PNG_HEX,
                "size_bytes": 16u64
            })),
            other => Err(LoomError::new(
                loom_rpc::error::LoomErrorCode::InvalidArgument,
                format!("unexpected method in fake: {other}"),
            )),
        }
    }
}

fn image_blocks(result: &ToolResult) -> Vec<&McpContent> {
    result
        .content
        .iter()
        .filter(|c| {
            let v = serde_json::to_value(c).unwrap_or(serde_json::Value::Null);
            v.get("type").and_then(|t| t.as_str()) == Some("image")
        })
        .collect()
}

#[tokio::test]
async fn web_screenshot_tool_result_includes_inline_png_image_block() {
    let rpc = RpcClient::with_caller_for_test(Box::new(ScreenshotFakeCaller));
    let result = rpc
        .call_as_tool_result("web.screenshot", serde_json::json!({"session": "s"}))
        .await;

    assert!(!result.is_error, "screenshot call should succeed");
    let imgs = image_blocks(&result);
    assert_eq!(
        imgs.len(),
        1,
        "expected exactly one inline image content block for web.screenshot, got content: {:?}",
        result.content
    );
    let v = serde_json::to_value(imgs[0]).unwrap();
    assert_eq!(
        v.get("mimeType").and_then(|m| m.as_str()),
        Some("image/png"),
        "image block must declare mimeType image/png"
    );
    let data = v
        .get("data")
        .and_then(|d| d.as_str())
        .expect("image block must carry base64 data");
    assert!(
        data.starts_with("iVBORw0KGg"),
        "image data must be base64 of a PNG (prefix iVBORw0KGg); got prefix {:?}",
        data.chars().take(12).collect::<String>()
    );
}

#[tokio::test]
async fn web_screenshot_tool_result_keeps_text_receipt_block() {
    // Wire-contract preservation: the receipt text block (with
    // screenshot_after_hash) must still be present for CLI/non-MCP parity.
    let rpc = RpcClient::with_caller_for_test(Box::new(ScreenshotFakeCaller));
    let result = rpc
        .call_as_tool_result("web.screenshot", serde_json::json!({"session": "s"}))
        .await;
    let has_hash_text = result.content.iter().any(|c| {
        let v = serde_json::to_value(c).unwrap_or(serde_json::Value::Null);
        v.get("type").and_then(|t| t.as_str()) == Some("text")
            && v.get("text")
                .and_then(|t| t.as_str())
                .map(|s| s.contains(FAKE_SHOT_HASH))
                .unwrap_or(false)
    });
    assert!(
        has_hash_text,
        "receipt text block carrying screenshot_after_hash must be preserved"
    );
}

// === Error-envelope decode preserves structured `data` as context (P2.3) ===
//
// The wire JSON-RPC error envelope carries structured detail under `data`
// (`JsonRpcError.data`), but `LoomError` deserializes its payload from
// `context`. `decode_error_envelope` must copy it across so the daemon's typed
// detail (e.g. session_cap_exceeded's {active, cap, hint}) reaches MCP callers
// structurally instead of dying inside the human message.

use crate::error_mapper::{ErrorMapper, TypedReceipt};

#[test]
fn decode_error_envelope_preserves_wire_data_as_context() {
    let envelope = json!({
        "code": "session_cap_exceeded",
        "message": "concurrent session cap reached (2/2)",
        "data": { "active": 2, "cap": 2, "hint": "close sessions or run `loom session reap`" }
    });
    let err = super::decode_error_envelope(&envelope);
    let ctx = err
        .context
        .expect("wire `data` must be preserved as LoomError::context");
    assert_eq!(ctx.get("active").and_then(|v| v.as_u64()), Some(2));
    assert_eq!(ctx.get("cap").and_then(|v| v.as_u64()), Some(2));
    assert!(ctx.get("hint").and_then(|v| v.as_str()).is_some());
}

#[test]
fn decode_error_envelope_without_data_has_no_context() {
    let envelope = json!({ "code": "invalid_argument", "message": "bad arg" });
    let err = super::decode_error_envelope(&envelope);
    assert!(
        err.context.is_none(),
        "absent wire `data` must leave context None"
    );
}

#[test]
fn mcp_error_path_surfaces_structured_cap_context_end_to_end() {
    // Decode (wire) -> map (TypedReceipt) is the full MCP error path minus the
    // socket: the structured cap detail must land in TypedReceipt.data.
    let envelope = json!({
        "code": "session_cap_exceeded",
        "message": "concurrent session cap reached (2/2); run `loom session reap`",
        "data": { "active": 2, "cap": 2, "hint": "close sessions or run `loom session reap`" }
    });
    let err = super::decode_error_envelope(&envelope);
    let result: ToolResult = ErrorMapper::to_tool_result(err);
    assert!(result.is_error);

    let text = match &result.content[0] {
        McpContent::Text { text } => text,
        other => panic!("expected text content block, got {other:?}"),
    };
    let receipt: TypedReceipt = serde_json::from_str(text).expect("parse TypedReceipt");
    let data = receipt.data.expect("data must carry structured cap detail");
    assert_eq!(
        data.get("active").and_then(|v| v.as_u64()),
        Some(2),
        "data={data}"
    );
    assert_eq!(
        data.get("cap").and_then(|v| v.as_u64()),
        Some(2),
        "data={data}"
    );
    assert!(data.get("hint").is_some(), "data={data}");
    assert_eq!(
        data.get("retry").and_then(|v| v.as_str()),
        Some("backoff"),
        "retry disposition overlaid alongside context; data={data}"
    );
}
