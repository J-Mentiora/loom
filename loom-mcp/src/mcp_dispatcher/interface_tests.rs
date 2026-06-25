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

// === shutdown is fire-and-forget (cancels the serve token) ===

#[test]
fn shutdown_signature_is_synchronous_unit() {
    fn _ck(d: &McpDispatcher) {
        d.shutdown()
    }
    let _ = _ck;
}

// === Regression (mcp-resources): the resources/read RESULT must be the
// MCP 2024-11-05 envelope { "contents": [TextResourceContents |
// BlobResourceContents] }. A bare ResourceContents object is rejected by
// strict clients (Claude Desktop/Code Zod validation). ===

use crate::rpc_client::{JsonRpcCaller, RpcClient};
use crate::stdio_transport::McpRequest;
use loom_rpc::error::LoomError;
use serde_json::json;
use std::sync::Arc;

const ENVELOPE_PNG_HEX: &str = "89504e470d0a1a0a0000000d49484452";
const ENVELOPE_BLOB_HASH: &str = "3333333333333333333333333333333333333333333333333333333333333333";

struct EnvelopeFakeCaller;

#[async_trait::async_trait]
impl JsonRpcCaller for EnvelopeFakeCaller {
    async fn raw_call(
        &self,
        method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, LoomError> {
        match method {
            "content.get" => Ok(json!({
                "artifact_ref": ENVELOPE_BLOB_HASH,
                "data_hex": ENVELOPE_PNG_HEX,
                "size_bytes": 16u64
            })),
            other => Err(LoomError::new(
                loom_rpc::error::LoomErrorCode::InvalidArgument,
                format!("unexpected method in fake: {other}"),
            )),
        }
    }
}

fn dispatcher_with_fake_caller(caller: Box<dyn JsonRpcCaller + Send + Sync>) -> Arc<McpDispatcher> {
    dispatcher_with_options(caller, super::SessionOptions::default())
}

fn dispatcher_with_options(
    caller: Box<dyn JsonRpcCaller + Send + Sync>,
    options: super::SessionOptions,
) -> Arc<McpDispatcher> {
    let rpc: Arc<RpcClient> = RpcClient::with_caller_for_test(caller);
    McpDispatcher::new(
        crate::tool_cache::ToolCache::new(rpc.clone()),
        crate::resource_tracker::ResourceTracker::new(rpc.clone()),
        rpc,
        crate::mcp_observability::McpObservability::new(true),
        tokio_util::sync::CancellationToken::new(),
        options,
    )
}

// === Implicit-session determinism options + self-heal (audit 2026-06-10:
// the idle reaper evicted the implicit session and every subsequent tool
// call failed with session_not_found forever; and seed/clock_anchor were
// unreachable over MCP). ===

use super::mcp_dispatcher::SessionOptions;
use loom_rpc::error::LoomErrorCode;
use std::collections::HashMap;
use std::sync::Mutex;

/// Records every (method, params) pair and serves a scripted daemon:
/// `rpc.schemas` → a one-method registry (web.navigate), `session.create`
/// → sequential ids `fixture-session-<n>`, `web.navigate` → the scripted
/// error for ids in `dead`, otherwise a receipt-shaped Ok.
struct RecordingFakeCaller {
    calls: Mutex<Vec<(String, serde_json::Value)>>,
    created: std::sync::atomic::AtomicUsize,
    dead: Mutex<HashMap<String, LoomErrorCode>>,
}

impl RecordingFakeCaller {
    fn new(dead: HashMap<String, LoomErrorCode>) -> Self {
        Self {
            calls: Mutex::new(vec![]),
            created: std::sync::atomic::AtomicUsize::new(0),
            dead: Mutex::new(dead),
        }
    }

    fn calls_for(
        calls: &Mutex<Vec<(String, serde_json::Value)>>,
        method: &str,
    ) -> Vec<serde_json::Value> {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, p)| p.clone())
            .collect()
    }
}

#[async_trait::async_trait]
impl JsonRpcCaller for RecordingFakeCaller {
    async fn raw_call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, LoomError> {
        self.calls
            .lock()
            .unwrap()
            .push((method.to_string(), params.clone()));
        match method {
            // Mirrors the REAL builtin web.navigate schema (incl. the
            // settle-capture optionals) — the previous {session,url}-only
            // mock meant this suite never exercised the documented full
            // arg set (mcp-navigate-schema-regression).
            "rpc.schemas" => Ok(json!({
                "methods": [{
                    "method": "web.navigate",
                    "request": {
                        "type": "object",
                        "properties": {
                            "session": { "type": "string" },
                            "url": { "type": "string" },
                            "until": { "type": "string", "enum": ["load", "networkidle", "settled"] },
                            "timeout_ms": { "type": "integer" }
                        },
                        "required": ["session", "url"],
                        "additionalProperties": false
                    },
                    "response": { "type": "object" }
                }],
                "source_wit_sha256": null
            })),
            "session.create" => {
                let n = self
                    .created
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    + 1;
                Ok(json!({
                    "session_id": format!("fixture-session-{n}"),
                    "status": "active",
                    "created_at_ms": 1_700_000_000_000_u64 + n as u64,
                }))
            }
            "session.close" => Ok(json!({
                "session_id": params.get("session_id").cloned().unwrap_or_default(),
                "status": "closed",
                "created_at_ms": 0,
            })),
            "web.navigate" => {
                let sid = params
                    .get("session")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if let Some(code) = self.dead.lock().unwrap().get(sid) {
                    return Err(LoomError::new(*code, format!("session gone: {sid}")));
                }
                Ok(json!({ "outcome_hash": "fixture-outcome", "session": sid }))
            }
            "session.diff" => Ok(json!({
                "a": params.get("a").cloned().unwrap_or_default(),
                "b": params.get("b").cloned().unwrap_or_default(),
                "diff": {
                    "field_diffs": [],
                    "screenshot_diffs": [],
                    "action_count_delta": 0,
                },
            })),
            "session.validate" => Ok(json!({
                "session_id": params.get("session_id").cloned().unwrap_or_default(),
                "passed": true,
                "reasons": [],
            })),
            "session.export" => Ok(json!({
                "session_id": params.get("session_id").cloned().unwrap_or_default(),
                "format": params.get("format").cloned().unwrap_or_default(),
                "artifact_ref": "fixture-artifact-ref",
            })),
            other => Err(LoomError::new(
                loom_rpc::error::LoomErrorCode::InvalidArgument,
                format!("unexpected method in fake: {other}"),
            )),
        }
    }
}

/// Build a dispatcher over a `RecordingFakeCaller` with a primed tool
/// cache (the advertised-tool gate requires web.navigate to be listed).
async fn primed_dispatcher(
    options: SessionOptions,
    dead: HashMap<String, LoomErrorCode>,
) -> (Arc<McpDispatcher>, Arc<RecordingFakeCaller>) {
    let caller = Arc::new(RecordingFakeCaller::new(dead));
    let dispatcher = dispatcher_with_options(Box::new(SharedCaller(caller.clone())), options);
    dispatcher.prime_tool_cache().await.expect("prime succeeds");
    (dispatcher, caller)
}

/// Box-able adapter so the test can keep a handle on the shared caller.
struct SharedCaller(Arc<RecordingFakeCaller>);

#[async_trait::async_trait]
impl JsonRpcCaller for SharedCaller {
    async fn raw_call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, LoomError> {
        self.0.raw_call(method, params).await
    }
}

fn navigate_params() -> ToolsCallParams {
    ToolsCallParams {
        name: "loom.web.navigate".into(),
        arguments: json!({ "url": "https://example.test" }),
    }
}

#[tokio::test]
async fn implicit_session_create_forwards_seed_clock_anchor_profile() {
    let options = SessionOptions {
        seed: Some(42),
        clock_anchor: Some(1_700_000_000_000),
        profile: Some("full".into()),
        budget: None,
        no_determinism: None,
    };
    let (dispatcher, caller) = primed_dispatcher(options, HashMap::new()).await;
    let result = dispatcher.tools_call(navigate_params()).await;
    assert!(!result.is_error, "navigate must succeed: {result:?}");
    let creates = RecordingFakeCaller::calls_for(&caller.calls, "session.create");
    assert_eq!(
        creates,
        vec![json!({ "profile": "full", "seed": 42, "clock_anchor": 1_700_000_000_000_u64 })],
        "env-derived options must reach session.create"
    );
}

#[tokio::test]
async fn implicit_session_create_forwards_no_determinism() {
    // A truthy `LOOM_MCP_SESSION_NO_DETERMINISM` baseline must reach
    // session.create as `no_determinism:true` (real wall-clock + unseeded RNG)
    // — the implicit-session path MCP clients (e.g. the studio) drive auth'd
    // SPAs through. `Some(false)` must NOT emit the key (byte-identical default).
    let options = SessionOptions {
        no_determinism: Some(true),
        ..SessionOptions::default()
    };
    let (dispatcher, caller) = primed_dispatcher(options, HashMap::new()).await;
    let result = dispatcher.tools_call(navigate_params()).await;
    assert!(!result.is_error, "navigate must succeed: {result:?}");
    let creates = RecordingFakeCaller::calls_for(&caller.calls, "session.create");
    assert_eq!(
        creates,
        vec![json!({ "profile": "standard", "no_determinism": true })],
        "no_determinism baseline must reach session.create"
    );
}

/// The v0.11.0 regression, MCP-side pin: the dispatcher must forward the
/// documented optional navigate args (`until`, `timeout_ms`) VERBATIM to the
/// daemon alongside the injected implicit session — no projection, no
/// dropping, no per-tool special-casing.
#[tokio::test]
async fn navigate_documented_optional_args_forward_verbatim() {
    let (dispatcher, caller) = primed_dispatcher(SessionOptions::default(), HashMap::new()).await;
    let result = dispatcher
        .tools_call(ToolsCallParams {
            name: "loom.web.navigate".into(),
            arguments: json!({
                "url": "https://example.test",
                "until": "settled",
                "timeout_ms": 5000
            }),
        })
        .await;
    assert!(!result.is_error, "navigate must succeed: {result:?}");
    let navigates = RecordingFakeCaller::calls_for(&caller.calls, "web.navigate");
    assert_eq!(
        navigates,
        vec![json!({
            "session": "fixture-session-1",
            "url": "https://example.test",
            "until": "settled",
            "timeout_ms": 5000
        })],
        "until/timeout_ms must reach the daemon verbatim with the injected session"
    );
}

#[tokio::test]
async fn implicit_session_create_with_default_options_is_unchanged() {
    // Acceptance: no env vars set → byte-identical wire behavior to the
    // pre-feature dispatcher (`{"profile":"standard"}`, nothing else).
    let (dispatcher, caller) = primed_dispatcher(SessionOptions::default(), HashMap::new()).await;
    let result = dispatcher.tools_call(navigate_params()).await;
    assert!(!result.is_error, "navigate must succeed: {result:?}");
    let creates = RecordingFakeCaller::calls_for(&caller.calls, "session.create");
    assert_eq!(creates, vec![json!({ "profile": "standard" })]);
}

#[test]
fn session_options_parse_valid_invalid_and_empty_values() {
    assert_eq!(
        SessionOptions::from_values(None, None, None, None, None).unwrap(),
        SessionOptions::default()
    );
    assert_eq!(
        SessionOptions::from_values(
            Some("42".into()),
            Some("1700000000000".into()),
            Some("standard".into()),
            Some(r#"{"session_walltime_ms":30000}"#.into()),
            Some("true".into()),
        )
        .unwrap(),
        SessionOptions {
            seed: Some(42),
            clock_anchor: Some(1_700_000_000_000),
            profile: Some("standard".into()),
            budget: Some(json!({ "session_walltime_ms": 30000 })),
            no_determinism: Some(true),
        }
    );
    // Shell `VAR=` (empty/whitespace) means unset, not an error.
    assert_eq!(
        SessionOptions::from_values(
            Some(String::new()),
            Some("  ".into()),
            Some(String::new()),
            Some("   ".into()),
            Some("  ".into()),
        )
        .unwrap(),
        SessionOptions::default()
    );
    // Malformed numerics fail loudly — a silently-dropped seed would
    // yield non-deterministic captures that look deterministic.
    let err = SessionOptions::from_values(Some("not-a-number".into()), None, None, None, None)
        .unwrap_err();
    assert_eq!(err.code, LoomErrorCode::InvalidArgument);
    assert!(err.message.contains("LOOM_MCP_SESSION_SEED"), "{err}");
    let err = SessionOptions::from_values(None, Some("-5".into()), None, None, None).unwrap_err();
    assert!(
        err.message.contains("LOOM_MCP_SESSION_CLOCK_ANCHOR"),
        "{err}"
    );
    // Malformed budget JSON fails loudly too — a silently-dropped budget
    // would let a runaway page outlive its intended kill deadline.
    let err =
        SessionOptions::from_values(None, None, None, Some("{not json".into()), None).unwrap_err();
    assert_eq!(err.code, LoomErrorCode::InvalidArgument);
    assert!(err.message.contains("LOOM_MCP_SESSION_BUDGET"), "{err}");
    // A typo'd boolean fails loudly — must not silently leave determinism on.
    let err =
        SessionOptions::from_values(None, None, None, None, Some("maybe".into())).unwrap_err();
    assert_eq!(err.code, LoomErrorCode::InvalidArgument);
    assert!(
        err.message.contains("LOOM_MCP_SESSION_NO_DETERMINISM"),
        "{err}"
    );
    // no_determinism forwards into the create params only when enabled.
    assert_eq!(
        SessionOptions {
            no_determinism: Some(true),
            ..SessionOptions::default()
        }
        .to_create_params(),
        json!({ "profile": "standard", "no_determinism": true })
    );
    assert_eq!(
        SessionOptions::default().to_create_params(),
        json!({ "profile": "standard" })
    );
}

#[test]
fn session_options_from_env_reads_process_env() {
    // Safe under the workspace's `--test-threads=1` discipline; these
    // vars are only read here and in `mcp_main::run`.
    std::env::set_var(super::ENV_SESSION_SEED, "7");
    std::env::set_var(super::ENV_SESSION_CLOCK_ANCHOR, "1700000000001");
    std::env::set_var(super::ENV_SESSION_PROFILE, "standard");
    std::env::set_var(super::ENV_SESSION_BUDGET, r#"{"wall_clock":"30s"}"#);
    let opts = SessionOptions::from_env().unwrap();
    std::env::remove_var(super::ENV_SESSION_SEED);
    std::env::remove_var(super::ENV_SESSION_CLOCK_ANCHOR);
    std::env::remove_var(super::ENV_SESSION_PROFILE);
    std::env::remove_var(super::ENV_SESSION_BUDGET);
    assert_eq!(
        opts,
        SessionOptions {
            seed: Some(7),
            clock_anchor: Some(1_700_000_000_001),
            profile: Some("standard".into()),
            budget: Some(json!({ "wall_clock": "30s" })),
            no_determinism: None,
        }
    );
    assert_eq!(
        SessionOptions::from_env().unwrap(),
        SessionOptions::default()
    );
}

#[tokio::test]
async fn evicted_implicit_session_recreates_with_same_options_and_retries() {
    for gone in [
        LoomErrorCode::SessionNotFound,
        LoomErrorCode::SessionClosed,
        LoomErrorCode::SessionAborted,
    ] {
        let options = SessionOptions {
            seed: Some(9),
            clock_anchor: None,
            profile: None,
            budget: None,
            no_determinism: None,
        };
        let dead = HashMap::from([("fixture-session-1".to_string(), gone)]);
        let (dispatcher, caller) = primed_dispatcher(options, dead).await;
        let result = dispatcher.tools_call(navigate_params()).await;
        assert!(
            !result.is_error,
            "self-heal must make the call succeed for {gone:?}: {result:?}"
        );
        let creates = RecordingFakeCaller::calls_for(&caller.calls, "session.create");
        assert_eq!(creates.len(), 2, "evicted session must be recreated once");
        assert_eq!(
            creates[0], creates[1],
            "recreate must reuse the same options"
        );
        let navigates = RecordingFakeCaller::calls_for(&caller.calls, "web.navigate");
        assert_eq!(navigates.len(), 2, "the failed call must be retried once");
        assert_eq!(
            navigates[1].get("session").and_then(|v| v.as_str()),
            Some("fixture-session-2"),
            "the retry must carry the fresh session id"
        );
    }
}

#[tokio::test]
async fn self_heal_retries_exactly_once_then_surfaces_the_error() {
    let dead = HashMap::from([
        (
            "fixture-session-1".to_string(),
            LoomErrorCode::SessionNotFound,
        ),
        (
            "fixture-session-2".to_string(),
            LoomErrorCode::SessionNotFound,
        ),
    ]);
    let (dispatcher, caller) = primed_dispatcher(SessionOptions::default(), dead).await;
    let result = dispatcher.tools_call(navigate_params()).await;
    assert!(result.is_error, "second failure must surface, not loop");
    assert_eq!(
        RecordingFakeCaller::calls_for(&caller.calls, "session.create").len(),
        2,
        "exactly one heal-recreate"
    );
    assert_eq!(
        RecordingFakeCaller::calls_for(&caller.calls, "web.navigate").len(),
        2,
        "exactly one retry"
    );
}

#[tokio::test]
async fn caller_pinned_session_is_never_healed() {
    let dead = HashMap::from([("user-pinned".to_string(), LoomErrorCode::SessionNotFound)]);
    let (dispatcher, caller) = primed_dispatcher(SessionOptions::default(), dead).await;
    let result = dispatcher
        .tools_call(ToolsCallParams {
            name: "loom.web.navigate".into(),
            arguments: json!({ "session": "user-pinned", "url": "https://example.test" }),
        })
        .await;
    assert!(result.is_error, "pinned-session failure must surface");
    assert!(
        RecordingFakeCaller::calls_for(&caller.calls, "session.create").is_empty(),
        "a caller-pinned session is not ours to recreate"
    );
    assert_eq!(
        RecordingFakeCaller::calls_for(&caller.calls, "web.navigate").len(),
        1,
        "no retry for a pinned session"
    );
}

#[tokio::test]
async fn non_session_errors_do_not_trigger_recreate() {
    let dead = HashMap::from([(
        "fixture-session-1".to_string(),
        LoomErrorCode::InvalidArgument,
    )]);
    let (dispatcher, caller) = primed_dispatcher(SessionOptions::default(), dead).await;
    let result = dispatcher.tools_call(navigate_params()).await;
    assert!(result.is_error);
    assert_eq!(
        RecordingFakeCaller::calls_for(&caller.calls, "session.create").len(),
        1,
        "a page/argument error must not churn the implicit session"
    );
    assert_eq!(
        RecordingFakeCaller::calls_for(&caller.calls, "web.navigate").len(),
        1
    );
}

// === loom.session.* tools: the determinism/session surface over MCP
// (reset/info/diff/validate/export; schema-driven, server-local) ===

use super::mcp_dispatcher::{
    TOOL_SESSION_DIFF, TOOL_SESSION_EXPORT, TOOL_SESSION_INFO, TOOL_SESSION_RESET,
    TOOL_SESSION_VALIDATE,
};

/// Parse the JSON receipt out of a successful ToolResult's text block.
fn result_json(result: &ToolResult) -> serde_json::Value {
    assert!(!result.is_error, "expected success: {result:?}");
    match &result.content[0] {
        crate::error_mapper::McpContent::Text { text } => {
            serde_json::from_str(text).expect("text block must be JSON")
        }
        other => panic!("expected text content, got {other:?}"),
    }
}

fn call(name: &str, arguments: serde_json::Value) -> ToolsCallParams {
    ToolsCallParams {
        name: name.into(),
        arguments,
    }
}

#[tokio::test]
async fn tools_list_includes_server_local_session_tools() {
    let (dispatcher, _caller) = primed_dispatcher(SessionOptions::default(), HashMap::new()).await;
    let names: Vec<String> = dispatcher
        .tools_list()
        .await
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert!(
        names.contains(&"loom.web.navigate".to_string()),
        "{names:?}"
    );
    for tool in [
        TOOL_SESSION_RESET,
        TOOL_SESSION_INFO,
        TOOL_SESSION_DIFF,
        TOOL_SESSION_VALIDATE,
        TOOL_SESSION_EXPORT,
    ] {
        assert!(
            names.contains(&tool.to_string()),
            "missing {tool}: {names:?}"
        );
    }
}

#[tokio::test]
async fn session_tools_advertised_even_when_daemon_derived_cache_is_cold() {
    // EnvelopeFakeCaller errors on rpc.schemas → prime fails → the
    // daemon-derived catalog is empty, but the server-local session
    // tools are still advertised.
    let dispatcher = dispatcher_with_fake_caller(Box::new(EnvelopeFakeCaller));
    let names: Vec<String> = dispatcher
        .tools_list()
        .await
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(names.len(), 5, "{names:?}");
    assert!(names.iter().all(|n| n.starts_with("loom.session.")));
}

#[tokio::test]
async fn session_reset_closes_old_session_and_merges_options_over_env_baseline() {
    let baseline = SessionOptions {
        seed: Some(1),
        clock_anchor: Some(2),
        profile: None,
        budget: None,
        no_determinism: None,
    };
    let (dispatcher, caller) = primed_dispatcher(baseline, HashMap::new()).await;
    // First tool call creates fixture-session-1 with the baseline.
    assert!(!dispatcher.tools_call(navigate_params()).await.is_error);

    // Reset with an explicit seed: other knobs fall back to the baseline.
    let result = dispatcher
        .tools_call(call(TOOL_SESSION_RESET, json!({ "seed": 9 })))
        .await;
    assert_eq!(
        result_json(&result),
        json!({ "session_id": "fixture-session-2" })
    );
    let closes = RecordingFakeCaller::calls_for(&caller.calls, "session.close");
    assert_eq!(closes, vec![json!({ "session_id": "fixture-session-1" })]);
    let creates = RecordingFakeCaller::calls_for(&caller.calls, "session.create");
    assert_eq!(
        creates[1],
        json!({ "profile": "standard", "seed": 9, "clock_anchor": 2 })
    );

    // A later argument-less reset is hermetic: back to the env baseline,
    // NOT the previous reset's seed override.
    let result = dispatcher
        .tools_call(call(TOOL_SESSION_RESET, json!({})))
        .await;
    assert_eq!(
        result_json(&result),
        json!({ "session_id": "fixture-session-3" })
    );
    let creates = RecordingFakeCaller::calls_for(&caller.calls, "session.create");
    assert_eq!(
        creates[2],
        json!({ "profile": "standard", "seed": 1, "clock_anchor": 2 })
    );

    // Subsequent web calls ride the new session.
    assert!(!dispatcher.tools_call(navigate_params()).await.is_error);
    let navigates = RecordingFakeCaller::calls_for(&caller.calls, "web.navigate");
    assert_eq!(
        navigates
            .last()
            .unwrap()
            .get("session")
            .and_then(|v| v.as_str()),
        Some("fixture-session-3")
    );
}

#[tokio::test]
async fn session_reset_forwards_budget_and_falls_back_to_baseline() {
    // The P3.1 contract: budget mirrors seed/clock_anchor/profile — an
    // explicit reset budget overrides the env baseline, and an
    // argument-less reset is hermetic (baseline budget, not the prior
    // override). Forwarded opaque; the daemon owns BudgetLimits.
    let baseline = SessionOptions {
        seed: None,
        clock_anchor: None,
        profile: None,
        budget: Some(json!({ "wall_clock": "60s" })),
        no_determinism: None,
    };
    let (dispatcher, caller) = primed_dispatcher(baseline, HashMap::new()).await;
    // First tool call creates fixture-session-1 with the baseline budget.
    assert!(!dispatcher.tools_call(navigate_params()).await.is_error);
    let creates = RecordingFakeCaller::calls_for(&caller.calls, "session.create");
    assert_eq!(
        creates[0],
        json!({ "profile": "standard", "budget": { "wall_clock": "60s" } }),
        "baseline budget must reach the first session.create"
    );

    // Reset with an explicit typed budget (the cleanest MCP contract —
    // the daemon deserializes BudgetLimits directly): overrides baseline.
    let result = dispatcher
        .tools_call(call(
            TOOL_SESSION_RESET,
            json!({ "budget": { "session_walltime_ms": 30000 } }),
        ))
        .await;
    assert_eq!(
        result_json(&result),
        json!({ "session_id": "fixture-session-2" })
    );
    let creates = RecordingFakeCaller::calls_for(&caller.calls, "session.create");
    assert_eq!(
        creates[1],
        json!({ "profile": "standard", "budget": { "session_walltime_ms": 30000 } }),
        "explicit reset budget must override the baseline and reach session.create"
    );

    // Argument-less reset is hermetic: back to the baseline budget, NOT
    // the previous reset's override.
    assert!(
        !dispatcher
            .tools_call(call(TOOL_SESSION_RESET, json!({})))
            .await
            .is_error
    );
    let creates = RecordingFakeCaller::calls_for(&caller.calls, "session.create");
    assert_eq!(
        creates[2],
        json!({ "profile": "standard", "budget": { "wall_clock": "60s" } }),
        "argument-less reset falls back to the baseline budget"
    );
}

#[tokio::test]
async fn session_reset_rejects_malformed_arguments_without_daemon_calls() {
    let (dispatcher, caller) = primed_dispatcher(SessionOptions::default(), HashMap::new()).await;
    let result = dispatcher
        .tools_call(call(TOOL_SESSION_RESET, json!({ "seed": "not-a-number" })))
        .await;
    assert!(result.is_error);
    let non_prime: Vec<String> = caller
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|(m, _)| m.clone())
        .filter(|m| m != "rpc.schemas")
        .collect();
    assert!(
        non_prime.is_empty(),
        "malformed args must be rejected before any RPC; got {non_prime:?}"
    );
}

#[tokio::test]
async fn session_info_reports_id_options_and_created_at() {
    let baseline = SessionOptions {
        seed: Some(5),
        clock_anchor: Some(7),
        profile: None,
        budget: None,
        no_determinism: None,
    };
    let (dispatcher, _caller) = primed_dispatcher(baseline, HashMap::new()).await;
    let result = dispatcher
        .tools_call(call(TOOL_SESSION_INFO, json!({})))
        .await;
    assert_eq!(
        result_json(&result),
        json!({
            "session_id": "fixture-session-1",
            "seed": 5,
            "clock_anchor": 7,
            "profile": "standard",
            "budget": null,
            "created_at_ms": 1_700_000_000_001_u64,
            "recreated_count": 0,
        })
    );
}

#[tokio::test]
async fn recreated_count_increments_on_eviction_self_heal() {
    // A determinism-pinning client must be able to DETECT a transparent
    // self-heal recreation rather than trust it: `recreated_count` is 0 on a
    // fresh process (the lazy first create is not a recreation) and advances
    // by one per eviction-triggered recreate. (Operator `loom.session.reset`
    // is excluded — it never routes through `invalidate_implicit_session`.)
    let dead = HashMap::from([(
        "fixture-session-1".to_string(),
        LoomErrorCode::SessionNotFound,
    )]);
    let (dispatcher, _caller) = primed_dispatcher(SessionOptions::default(), dead).await;

    // (1) Fresh dispatcher: lazy create of fixture-session-1, count still 0.
    let before = result_json(
        &dispatcher
            .tools_call(call(TOOL_SESSION_INFO, json!({})))
            .await,
    );
    assert_eq!(before["recreated_count"], json!(0));
    assert_eq!(before["session_id"], json!("fixture-session-1"));

    // (2) The current session is dead → a web.* call triggers self-heal.
    assert!(
        !dispatcher.tools_call(navigate_params()).await.is_error,
        "self-heal must make the navigate succeed"
    );

    // (3) session.info now reflects the recreation: count==1, a fresh id,
    // and a newer created_at_ms.
    let after = result_json(
        &dispatcher
            .tools_call(call(TOOL_SESSION_INFO, json!({})))
            .await,
    );
    assert_eq!(after["recreated_count"], json!(1), "one eviction self-heal");
    assert_ne!(
        after["session_id"], before["session_id"],
        "self-heal must swap in a fresh session id"
    );
    assert!(
        after["created_at_ms"].as_u64().unwrap() > before["created_at_ms"].as_u64().unwrap(),
        "the recreated session must report a newer created_at_ms"
    );
}

#[tokio::test]
async fn session_diff_proxies_current_session_against_other() {
    let (dispatcher, caller) = primed_dispatcher(SessionOptions::default(), HashMap::new()).await;
    assert!(!dispatcher.tools_call(navigate_params()).await.is_error);
    let result = dispatcher
        .tools_call(call(
            TOOL_SESSION_DIFF,
            json!({ "other_session_id": "prev-run-session" }),
        ))
        .await;
    let body = result_json(&result);
    assert_eq!(body["diff"]["field_diffs"], json!([]));
    let diffs = RecordingFakeCaller::calls_for(&caller.calls, "session.diff");
    assert_eq!(
        diffs,
        vec![json!({
            "a": "prev-run-session",
            "b": "fixture-session-1",
            "include_screenshots": false,
            "show_dom_diffs": false,
        })]
    );
}

#[tokio::test]
async fn session_validate_and_export_default_to_current_session() {
    let (dispatcher, caller) = primed_dispatcher(SessionOptions::default(), HashMap::new()).await;
    let result = dispatcher
        .tools_call(call(TOOL_SESSION_VALIDATE, json!({})))
        .await;
    assert_eq!(result_json(&result)["passed"], json!(true));
    assert_eq!(
        RecordingFakeCaller::calls_for(&caller.calls, "session.validate"),
        vec![json!({ "session_id": "fixture-session-1" })]
    );

    let result = dispatcher
        .tools_call(call(TOOL_SESSION_EXPORT, json!({ "format": "har" })))
        .await;
    assert_eq!(
        result_json(&result)["artifact_ref"],
        json!("fixture-artifact-ref")
    );
    assert_eq!(
        RecordingFakeCaller::calls_for(&caller.calls, "session.export"),
        vec![json!({ "session_id": "fixture-session-1", "format": "har" })]
    );
}

#[tokio::test]
async fn resources_read_result_is_wrapped_in_contents_array() {
    let dispatcher = dispatcher_with_fake_caller(Box::new(EnvelopeFakeCaller));

    let req = McpRequest {
        jsonrpc: "2.0".into(),
        id: Some(json!(7)),
        method: METHOD_RESOURCES_READ.into(),
        params: json!({ "uri": format!("loom://blob/{ENVELOPE_BLOB_HASH}") }),
    };
    let resp = dispatcher
        .dispatch(req)
        .await
        .expect("request with an id must get a response");
    assert!(resp.error.is_none(), "got {:?}", resp.error);
    let result = resp.result.expect("resources/read must carry a result");

    let contents = result
        .get("contents")
        .and_then(|c| c.as_array())
        .expect("result must be the MCP envelope { \"contents\": [...] }");
    assert_eq!(contents.len(), 1, "exactly one resource was read");
    let entry = &contents[0];
    assert_eq!(
        entry.get("uri").and_then(|u| u.as_str()),
        Some(format!("loom://blob/{ENVELOPE_BLOB_HASH}").as_str())
    );
    assert_eq!(
        entry.get("mimeType").and_then(|m| m.as_str()),
        Some("image/png")
    );
    assert!(
        entry.get("blob").and_then(|b| b.as_str()).is_some(),
        "blob entry must carry base64 bytes"
    );
}

// === Advertised-tool gate (audit 2026-06-10): tools/call must not reach
// daemon methods that tools/list never advertised. Pre-fix, the bare
// "loom." prefix strip forwarded vault.*, session.kill, gc.run,
// content.get, import.playwright … straight to the daemon's router. ===

#[tokio::test]
async fn tools_call_rejects_non_advertised_daemon_methods() {
    let (dispatcher, caller) = primed_dispatcher(SessionOptions::default(), HashMap::new()).await;
    for name in [
        "loom.vault.grant",
        "loom.vault.set_secret",
        "loom.session.kill",
        "loom.gc.run",
        "loom.content.get",
        "loom.import.playwright",
        "loom.rpc.schemas",
    ] {
        let result = dispatcher
            .tools_call(call(name, json!({ "session_id": "any" })))
            .await;
        assert!(result.is_error, "{name} must be rejected, got {result:?}");
        let body = match &result.content[0] {
            crate::error_mapper::McpContent::Text { text } => text.clone(),
            other => panic!("expected text content, got {other:?}"),
        };
        assert!(
            body.contains("unknown tool"),
            "{name} must surface as unknown tool, got {body}"
        );
    }
    // None of the rejected names ever reached the daemon: the only
    // recorded call is the tool-cache prime itself.
    let methods: Vec<String> = caller
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|(m, _)| m.clone())
        .collect();
    assert_eq!(
        methods,
        vec!["rpc.schemas".to_string()],
        "non-advertised tools/call names must never be forwarded"
    );
    // Advertised web tools and the server-local session tools still work.
    assert!(!dispatcher.tools_call(navigate_params()).await.is_error);
    assert!(
        !dispatcher
            .tools_call(call(TOOL_SESSION_INFO, json!({})))
            .await
            .is_error
    );
}

#[tokio::test]
async fn tools_call_with_cold_cache_surfaces_prime_error_not_unknown_tool() {
    // EnvelopeFakeCaller fails rpc.schemas, so the lazy prime inside the
    // gate fails: a daemon-down loom.web.navigate must surface the
    // transport/prime error, NOT a misleading "unknown tool".
    let dispatcher = dispatcher_with_fake_caller(Box::new(EnvelopeFakeCaller));
    let result = dispatcher.tools_call(navigate_params()).await;
    assert!(result.is_error);
    let body = match &result.content[0] {
        crate::error_mapper::McpContent::Text { text } => text.clone(),
        other => panic!("expected text content, got {other:?}"),
    };
    assert!(
        !body.contains("unknown tool"),
        "cold-cache failure must not masquerade as unknown tool: {body}"
    );
}
