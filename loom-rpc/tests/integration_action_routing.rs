//! Integration test.
//!
//! Verifies the full `request_router → rpc_handlers → HostServiceAdapter`
//! dispatch chain for `web.*` action methods:
//!
//! - `rpc.schemas` response includes all 11 web.* methods.
//! - `web.navigate` dispatches through the chain and returns
//!   a canned `Receipt` via JSON-RPC, NOT "method not found".
//!
//! Uses a `WebSchemas` stub (all 11 web.* method schemas in-memory) and a
//! `CannedHostBridge` that returns a fixed `Receipt` so the test is hermetic.

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use loom_rpc::{
    auth_middleware::auth_middleware::{AuthMiddleware, Token},
    connection_handler::connection_handler::ConnectionHandlerDeps,
    frame_handler::frame_handler::FrameHandler,
    host_service_adapter::host_service_adapter::{
        Action, HostServiceAdapter, Receipt, ReceiptStatus, WasmHostBridge,
    },
    request_router::request_router::RequestRouter,
    rpc_handlers::rpc_handlers::RpcHandlers,
    rpc_observability::rpc_observability::RpcObservability,
    schema_provider::schema_provider::{
        CompiledJsonSchema, MethodSchema, SchemaProviderApi, SchemaRegistry,
    },
    schema_validator::schema_validator::SchemaValidator,
    socket_server::socket_server::{SocketServer, SocketServerConfig},
};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::net::UnixStream;

mod common;

// ─── Web schema definitions (all 11 web.* methods) ───────────────────────────
//
// Source: loom-cli/src/postinstall_runner/interfaces.rs (canonical schema spec).
// Used inline here so the integration test is self-contained and doesn't
// depend on a schema directory on disk.

static WEB_SCHEMAS: &[(&str, &str, &str)] = &[
    (
        "web.navigate",
        r#"{"type":"object","properties":{"session_id":{"type":"string"},"session":{"type":"string"},"deadline_ms":{"type":"integer"},"url":{"type":"string"}},"required":["url"],"additionalProperties":false}"#,
        r#"{"type":"object","properties":{"action_id":{"type":"integer"},"session_id":{"type":"string"},"status":{"type":"string"}},"required":["action_id","session_id","status"]}"#,
    ),
    (
        "web.click",
        r#"{"type":"object","properties":{"session_id":{"type":"string"},"session":{"type":"string"},"deadline_ms":{"type":"integer"},"selector":{"type":"string"}},"required":["selector"],"additionalProperties":false}"#,
        r#"{"type":"object","properties":{"action_id":{"type":"integer"},"session_id":{"type":"string"},"status":{"type":"string"}},"required":["action_id","session_id","status"]}"#,
    ),
    (
        // Canonical name `web.type` (was `web.type_text`); the legacy
        // spelling resolves here via `loom_shared::action_aliases`.
        "web.type",
        r#"{"type":"object","properties":{"session_id":{"type":"string"},"session":{"type":"string"},"deadline_ms":{"type":"integer"},"selector":{"type":"string"},"text":{"type":"string"}},"required":["selector","text"],"additionalProperties":false}"#,
        r#"{"type":"object","properties":{"action_id":{"type":"integer"},"session_id":{"type":"string"},"status":{"type":"string"}},"required":["action_id","session_id","status"]}"#,
    ),
    (
        "web.select",
        r#"{"type":"object","properties":{"session_id":{"type":"string"},"session":{"type":"string"},"deadline_ms":{"type":"integer"},"selector":{"type":"string"},"value":{"type":"string"}},"required":["selector","value"],"additionalProperties":false}"#,
        r#"{"type":"object","properties":{"action_id":{"type":"integer"},"session_id":{"type":"string"},"status":{"type":"string"}},"required":["action_id","session_id","status"]}"#,
    ),
    (
        "web.hover",
        r#"{"type":"object","properties":{"session_id":{"type":"string"},"session":{"type":"string"},"deadline_ms":{"type":"integer"},"selector":{"type":"string"}},"required":["selector"],"additionalProperties":false}"#,
        r#"{"type":"object","properties":{"action_id":{"type":"integer"},"session_id":{"type":"string"},"status":{"type":"string"}},"required":["action_id","session_id","status"]}"#,
    ),
    (
        "web.scroll",
        r#"{"type":"object","properties":{"session_id":{"type":"string"},"session":{"type":"string"},"deadline_ms":{"type":"integer"},"selector":{"type":"string"},"delta_x":{"type":"integer"},"delta_y":{"type":"integer"}},"additionalProperties":false}"#,
        r#"{"type":"object","properties":{"action_id":{"type":"integer"},"session_id":{"type":"string"},"status":{"type":"string"}},"required":["action_id","session_id","status"]}"#,
    ),
    (
        "web.wait",
        r#"{"type":"object","properties":{"session_id":{"type":"string"},"session":{"type":"string"},"deadline_ms":{"type":"integer"},"selector":{"type":"string"},"timeout_ms":{"type":"integer"}},"required":["selector"],"additionalProperties":false}"#,
        r#"{"type":"object","properties":{"action_id":{"type":"integer"},"session_id":{"type":"string"},"status":{"type":"string"}},"required":["action_id","session_id","status"]}"#,
    ),
    (
        // settle-capture readiness verb; the SDKs spell it
        // `action.web.wait_for` (resolved via `loom_shared::action_aliases`).
        "web.wait_for",
        r#"{"type":"object","properties":{"session_id":{"type":"string"},"session":{"type":"string"},"deadline_ms":{"type":"integer"},"until":{"type":"string","enum":["load","networkidle","settled"]},"timeout_ms":{"type":"integer"}},"additionalProperties":false}"#,
        r#"{"type":"object","properties":{"action_id":{"type":"integer"},"session_id":{"type":"string"},"status":{"type":"string"}},"required":["action_id","session_id","status"]}"#,
    ),
    (
        "web.evaluate",
        r#"{"type":"object","properties":{"session_id":{"type":"string"},"session":{"type":"string"},"deadline_ms":{"type":"integer"},"expression":{"type":"string"}},"required":["expression"],"additionalProperties":false}"#,
        r#"{"type":"object","properties":{"action_id":{"type":"integer"},"session_id":{"type":"string"},"status":{"type":"string"}},"required":["action_id","session_id","status"]}"#,
    ),
    (
        "web.screenshot",
        r#"{"type":"object","properties":{"session_id":{"type":"string"},"session":{"type":"string"},"deadline_ms":{"type":"integer"},"selector":{"type":"string"}},"additionalProperties":false}"#,
        r#"{"type":"object","properties":{"action_id":{"type":"integer"},"session_id":{"type":"string"},"status":{"type":"string"}},"required":["action_id","session_id","status"]}"#,
    ),
    (
        "web.snapshot",
        r#"{"type":"object","properties":{"session_id":{"type":"string"},"session":{"type":"string"},"deadline_ms":{"type":"integer"}},"additionalProperties":false}"#,
        r#"{"type":"object","properties":{"action_id":{"type":"integer"},"session_id":{"type":"string"},"status":{"type":"string"}},"required":["action_id","session_id","status"]}"#,
    ),
];

// ─── WebSchemas stub ──────────────────────────────────────────────────────────

struct WebSchemas {
    request_schemas: std::collections::HashMap<String, Arc<CompiledJsonSchema>>,
    response_schemas: std::collections::HashMap<String, Arc<CompiledJsonSchema>>,
    snapshot: SchemaRegistry,
}

impl WebSchemas {
    fn new() -> Arc<Self> {
        let mut request_schemas = std::collections::HashMap::new();
        let mut response_schemas = std::collections::HashMap::new();
        let mut method_schemas = Vec::new();

        for &(method, req_json, resp_json) in WEB_SCHEMAS {
            let req: serde_json::Value = serde_json::from_str(req_json).unwrap();
            let resp: serde_json::Value = serde_json::from_str(resp_json).unwrap();
            request_schemas.insert(
                method.to_string(),
                Arc::new(
                    CompiledJsonSchema::compile(req.clone()).expect("test schema must compile"),
                ),
            );
            response_schemas.insert(
                method.to_string(),
                Arc::new(
                    CompiledJsonSchema::compile(resp.clone()).expect("test schema must compile"),
                ),
            );
            let aliases: Vec<String> = loom_shared::action_aliases::aliases_of(method)
                .into_iter()
                .map(|s| s.to_string())
                .collect();
            method_schemas.push(MethodSchema {
                method: method.to_string(),
                request: req,
                response: resp,
                aliases,
            });
        }

        method_schemas.sort_by(|a, b| a.method.cmp(&b.method));
        let snapshot = SchemaRegistry {
            methods: method_schemas,
            source_wit_sha256: "test-stub".to_string(),
        };

        Arc::new(Self {
            request_schemas,
            response_schemas,
            snapshot,
        })
    }
}

impl SchemaProviderApi for WebSchemas {
    fn lookup_request_schema(&self, method: &str) -> Option<Arc<CompiledJsonSchema>> {
        self.request_schemas.get(method).cloned()
    }
    fn lookup_response_schema(&self, method: &str) -> Option<Arc<CompiledJsonSchema>> {
        self.response_schemas.get(method).cloned()
    }
    fn registered_methods(&self) -> Vec<String> {
        // Include rpc.schemas (built-in) plus all web.* methods.
        let mut methods: Vec<String> = self.request_schemas.keys().cloned().collect();
        methods.push("rpc.schemas".to_string());
        methods.sort();
        methods
    }
    fn get_registry_snapshot(&self) -> SchemaRegistry {
        self.snapshot.clone()
    }
}

// ─── CannedHostBridge stub ────────────────────────────────────────────────────

/// Returns a fixed Receipt for any action dispatch. Used to
/// verify the chain returns the receipt rather than "method not found".
struct CannedHostBridge;

impl WasmHostBridge for CannedHostBridge {
    fn dispatch_action_blocking(
        &self,
        action: Action,
        _deadline_ms: Option<u64>,
    ) -> Result<Receipt, loom_rpc::host_service_adapter::host_service_adapter::AdapterError> {
        let session_id = match &action {
            Action::WebNavigate { session_id, .. }
            | Action::WebClick { session_id, .. }
            | Action::WebEvaluate { session_id, .. }
            | Action::WebType { session_id, .. }
            | Action::WebScreenshot { session_id, .. }
            | Action::WebSelect { session_id, .. }
            | Action::WebHover { session_id, .. }
            | Action::WebScroll { session_id, .. }
            | Action::WebWait { session_id, .. }
            | Action::WebSnapshot { session_id } => session_id.clone(),
            Action::WebStartRecording { session_id, .. }
            | Action::WebStopRecording { session_id } => session_id.clone(),
            Action::WebWaitFor { session_id, .. } => session_id.clone(),
            Action::WebSetInputFiles { session_id, .. } => session_id.clone(),
            // v0.9.6 cookie verbs.
            Action::WebSetCookies { session_id, .. }
            | Action::WebGetCookies { session_id, .. }
            | Action::WebClearCookies { session_id }
            | Action::WebDeleteCookies { session_id, .. }
            | Action::WebNetworkLog { session_id } => session_id.clone(),
            Action::WebPressKey { session_id, .. } => session_id.clone(),
        };
        Ok(Receipt {
            action_id: 42,
            session_id,
            status: ReceiptStatus::Success,
            timing_ticks: 100,
            side_effects: vec![],
            error: None,
            action_hash: None,
            outcome_hash: None,
            emitted_at_ms: None,
            url: None,
            final_url: None,
            title: None,
            status_code: None,
            dom_snapshot_hash: None,
            dom_after_hash: None,
            screenshot_after_hash: None,
            screencast_after_hash: None,
            console_count: None,
            network_count: None,
            console_lines: vec![],
            network_summary: None,
            network_entries: vec![],
            network_entries_blob_ref: None,
            network_entries_truncated: None,
            settle_until: None,
            settle_outcome: None,
            return_value_json: None,
            return_value_blob_ref: None,
            set_cookies_result: None,
            get_cookies_result: None,
            clear_cookies_result: None,
            delete_cookies_result: None,
            scroll_result: None,
        })
    }
}

// ─── NoopCoreBridge ───────────────────────────────────────────────────────────

struct NoopCoreBridge;
impl loom_rpc::core_service_adapter::core_service_adapter::CoreFacadeBridge for NoopCoreBridge {
    fn export_session_to_cas(
        &self,
        _s: &str,
        _f: &str,
    ) -> Result<
        loom_rpc::core_service_adapter::core_service_adapter::ExportInfo,
        loom_rpc::core_service_adapter::core_service_adapter::AdapterError,
    > {
        Err(loom_rpc::error_translator::error_translator::LoomErrorCode::InternalError)
    }
    fn get_export_bytes(
        &self,
        _r: &str,
    ) -> Result<Vec<u8>, loom_rpc::core_service_adapter::core_service_adapter::AdapterError> {
        Err(loom_rpc::error_translator::error_translator::LoomErrorCode::InternalError)
    }
    fn list_sessions_info(
        &self,
    ) -> Result<
        Vec<(String, String, u64)>,
        loom_rpc::core_service_adapter::core_service_adapter::AdapterError,
    > {
        Ok(vec![])
    }
    fn replay_session_to_id(
        &self,
        _s: &str,
    ) -> Result<String, loom_rpc::error_translator::error_translator::LoomError> {
        Err(
            loom_rpc::error_translator::error_translator::LoomError::new(
                loom_rpc::error_translator::error_translator::LoomErrorCode::InternalError,
                "",
            ),
        )
    }
    fn diff_sessions_json(
        &self,
        _a: &str,
        _b: &str,
        _i: bool,
    ) -> Result<serde_json::Value, loom_rpc::core_service_adapter::core_service_adapter::AdapterError>
    {
        Err(loom_rpc::error_translator::error_translator::LoomErrorCode::InternalError)
    }
    fn inspect_session_json(
        &self,
        _s: &str,
        _at: Option<u64>,
    ) -> Result<serde_json::Value, loom_rpc::core_service_adapter::core_service_adapter::AdapterError>
    {
        Err(loom_rpc::error_translator::error_translator::LoomErrorCode::InternalError)
    }
    fn validate_session_result(
        &self,
        _s: &str,
    ) -> Result<
        loom_rpc::core_service_adapter::core_service_adapter::ValidationResult,
        loom_rpc::core_service_adapter::core_service_adapter::AdapterError,
    > {
        Err(loom_rpc::error_translator::error_translator::LoomErrorCode::InternalError)
    }
    fn import_playwright_from_bytes(
        &self,
        _: &[u8],
    ) -> Result<
        loom_rpc::core_service_adapter::core_service_adapter::PlaywrightImportInfo,
        loom_rpc::core_service_adapter::core_service_adapter::AdapterError,
    > {
        Err(loom_rpc::error_translator::error_translator::LoomErrorCode::InternalError)
    }
    fn create_session_raw(
        &self,
        _: loom_rpc::core_service_adapter::core_service_adapter::CreateSessionParams,
    ) -> Result<(String, u64), loom_rpc::core_service_adapter::core_service_adapter::LoomError>
    {
        Err(loom_rpc::error_translator::error_translator::LoomErrorCode::InternalError.into())
    }
    fn close_session_raw(
        &self,
        _: &str,
    ) -> Result<(), loom_rpc::core_service_adapter::core_service_adapter::AdapterError> {
        Err(loom_rpc::error_translator::error_translator::LoomErrorCode::InternalError)
    }
    fn abort_session_raw(
        &self,
        _: &str,
        _: &str,
    ) -> Result<(), loom_rpc::core_service_adapter::core_service_adapter::AdapterError> {
        Err(loom_rpc::error_translator::error_translator::LoomErrorCode::InternalError)
    }
    fn vault_grant(
        &self,
        _: loom_rpc::core_service_adapter::core_service_adapter::GrantParams,
    ) -> Result<
        loom_rpc::core_service_adapter::core_service_adapter::GrantInfo,
        loom_rpc::core_service_adapter::core_service_adapter::AdapterError,
    > {
        Err(loom_rpc::error_translator::error_translator::LoomErrorCode::InternalError)
    }
    fn vault_revoke(
        &self,
        _: &str,
        _: &str,
    ) -> Result<(), loom_rpc::core_service_adapter::core_service_adapter::AdapterError> {
        Err(loom_rpc::error_translator::error_translator::LoomErrorCode::InternalError)
    }
    fn vault_list_grants(
        &self,
        _: Option<&str>,
    ) -> Result<
        Vec<loom_rpc::core_service_adapter::core_service_adapter::GrantInfo>,
        loom_rpc::core_service_adapter::core_service_adapter::AdapterError,
    > {
        Ok(vec![])
    }
    fn vault_add(
        &self,
        _: loom_rpc::core_service_adapter::core_service_adapter::VaultAddParams,
    ) -> Result<
        loom_rpc::core_service_adapter::core_service_adapter::VaultAddInfo,
        loom_rpc::core_service_adapter::core_service_adapter::AdapterError,
    > {
        Err(loom_rpc::error_translator::error_translator::LoomErrorCode::InternalError)
    }
    fn vault_set_secret(
        &self,
        _: loom_rpc::core_service_adapter::core_service_adapter::VaultSetSecretParams,
    ) -> Result<
        loom_rpc::core_service_adapter::core_service_adapter::VaultSetSecretInfo,
        loom_rpc::core_service_adapter::core_service_adapter::AdapterError,
    > {
        Err(loom_rpc::error_translator::error_translator::LoomErrorCode::InternalError)
    }
    fn vault_delete_secret(
        &self,
        _: loom_rpc::core_service_adapter::core_service_adapter::VaultDeleteSecretParams,
    ) -> Result<
        loom_rpc::core_service_adapter::core_service_adapter::VaultDeleteSecretInfo,
        loom_rpc::core_service_adapter::core_service_adapter::AdapterError,
    > {
        Err(loom_rpc::error_translator::error_translator::LoomErrorCode::InternalError)
    }
    fn vault_list_labels(
        &self,
        _: loom_rpc::core_service_adapter::core_service_adapter::VaultListLabelsParams,
    ) -> Result<
        loom_rpc::core_service_adapter::core_service_adapter::VaultListLabelsInfo,
        loom_rpc::core_service_adapter::core_service_adapter::AdapterError,
    > {
        Ok(
            loom_rpc::core_service_adapter::core_service_adapter::VaultListLabelsInfo {
                labels: vec![],
                count: 0,
            },
        )
    }
    fn vault_diagnose(
        &self,
    ) -> Result<
        loom_rpc::core_service_adapter::core_service_adapter::VaultDiagnoseInfo,
        loom_rpc::core_service_adapter::core_service_adapter::AdapterError,
    > {
        Err(loom_rpc::error_translator::error_translator::LoomErrorCode::InternalError)
    }

    fn vault_get_session_context(
        &self,
    ) -> Result<
        loom_rpc::core_service_adapter::core_service_adapter::VaultGetSessionContextInfo,
        loom_rpc::error_translator::error_translator::LoomErrorCode,
    > {
        Err(loom_rpc::error_translator::error_translator::LoomErrorCode::InternalError)
    }
    fn gc_run(
        &self,
        _: Option<u64>,
        _: Option<u64>,
    ) -> Result<
        loom_rpc::core_service_adapter::core_service_adapter::GcRunReport,
        loom_rpc::core_service_adapter::core_service_adapter::AdapterError,
    > {
        Ok(loom_rpc::core_service_adapter::core_service_adapter::GcRunReport::default())
    }
    fn session_reap(
        &self,
        _: bool,
    ) -> Result<
        loom_rpc::core_service_adapter::core_service_adapter::ReapReport,
        loom_rpc::core_service_adapter::core_service_adapter::AdapterError,
    > {
        Ok(loom_rpc::core_service_adapter::core_service_adapter::ReapReport::default())
    }
}

// ─── Test server ──────────────────────────────────────────────────────────────

struct ActionTestServer {
    _dir: TempDir,
    token: Token,
    socket_path: std::path::PathBuf,
}

async fn start_action_server() -> (ActionTestServer, tokio::task::JoinHandle<()>) {
    // Hold the bind lock across TempDir creation through the bind (see common::BIND_LOCK);
    // dropped before the first `.await` so the future stays Send.
    let bind_lock = common::bind_guard();
    let dir = TempDir::new().unwrap();
    let socket_path = dir.path().join("action_test.sock");
    let token = Token::generate();
    let token_arc = Arc::new(token.clone());

    let schemas: Arc<dyn SchemaProviderApi> = WebSchemas::new();
    let validator: Arc<dyn loom_rpc::schema_validator::schema_validator::SchemaValidatorApi> =
        SchemaValidator::new(Arc::clone(&schemas));
    let obs: Arc<dyn loom_rpc::rpc_observability::rpc_observability::RpcObservabilityApi> =
        RpcObservability::new();
    let auth: Arc<dyn loom_rpc::auth_middleware::auth_middleware::AuthMiddlewareApi> =
        AuthMiddleware::new(Arc::clone(&token_arc));

    let core_adapter =
        loom_rpc::core_service_adapter::core_service_adapter::CoreServiceAdapter::new(Arc::new(
            NoopCoreBridge,
        )
            as Arc<dyn loom_rpc::core_service_adapter::core_service_adapter::CoreFacadeBridge>);
    let host_adapter =
        HostServiceAdapter::new(Arc::new(CannedHostBridge) as Arc<dyn WasmHostBridge>);

    let handlers = RpcHandlers::new(
        core_adapter,
        host_adapter,
        Arc::clone(&schemas),
        Arc::clone(&validator),
        Arc::clone(&obs),
    );
    let router: Arc<dyn loom_rpc::request_router::request_router::RequestRouterApi> =
        RequestRouter::register_methods(handlers, Arc::clone(&schemas), Arc::clone(&validator))
            .expect("router registration must succeed");

    let deps = Arc::new(ConnectionHandlerDeps {
        auth,
        validator,
        router,
        observability: obs,
    });
    let cfg = SocketServerConfig {
        socket_path: socket_path.clone(),
        token_override: Some(token.clone()),
    };
    let server = SocketServer::new(cfg, deps).expect("SocketServer::new must succeed");
    drop(bind_lock); // bind done — release before any `.await`
    let handle = tokio::runtime::Handle::current();
    let join = tokio::spawn(async move {
        server.serve(handle, futures::future::pending::<()>()).await;
    });

    (
        ActionTestServer {
            _dir: dir,
            token,
            socket_path,
        },
        join,
    )
}

async fn send_frame(
    framed: &mut tokio_util::codec::Framed<UnixStream, tokio_util::codec::LengthDelimitedCodec>,
    data: &[u8],
) {
    framed.send(Bytes::copy_from_slice(data)).await.unwrap();
}

async fn recv_frame(
    framed: &mut tokio_util::codec::Framed<UnixStream, tokio_util::codec::LengthDelimitedCodec>,
) -> Vec<u8> {
    let b = framed
        .next()
        .await
        .expect("frame expected")
        .expect("no codec error");
    b.to_vec()
}

async fn connect(
    socket_path: &std::path::Path,
) -> tokio_util::codec::Framed<UnixStream, tokio_util::codec::LengthDelimitedCodec> {
    let stream = UnixStream::connect(socket_path)
        .await
        .expect("connect must succeed");
    FrameHandler::wrap_stream(stream)
}

// ─── Test 1: web.navigate returns canned Receipt ───────────────

/// HELLO + rpc.schemas + web.navigate with stubbed HostServiceAdapter
/// must return the canned Receipt, NOT "method not found".
#[tokio::test(flavor = "multi_thread")]
async fn test_action_routing_web_navigate_returns_receipt() {
    let (srv, _bg) = start_action_server().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut framed = connect(&srv.socket_path).await;

    // Authenticate
    send_frame(&mut framed, format!("HELLO {}", srv.token.0).as_bytes()).await;

    // Send web.navigate
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "web.navigate",
        "params": {
            "session_id": "test-session-01",
            "url": "https://example.com"
        }
    });
    send_frame(&mut framed, request.to_string().as_bytes()).await;

    // Expect the canned Receipt (action_id: 42, status: "success")
    let resp_bytes = recv_frame(&mut framed).await;
    let resp: serde_json::Value =
        serde_json::from_slice(&resp_bytes).expect("web.navigate response must be valid JSON");

    // Must NOT be a "method not found" error
    if let Some(err) = resp.get("error") {
        let msg = err.get("message").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            !msg.contains("method not found"),
            "web.navigate must NOT return 'method not found'; got error: {err}"
        );
    }

    // Must be a success result containing the receipt fields
    let result = resp
        .get("result")
        .expect("web.navigate must return a result; got no 'result' field in response");
    assert_eq!(
        result.get("action_id").and_then(|v| v.as_u64()),
        Some(42),
        "expected canned action_id=42 from CannedHostBridge; got: {result}"
    );
    assert_eq!(
        result.get("status").and_then(|v| v.as_str()),
        Some("success"),
        "expected canned status=success from CannedHostBridge; got: {result}"
    );
}

// ─── Test 2: rpc.schemas includes all 11 web.* methods ─────────

/// rpc.schemas response must list all 11 web.* methods.
#[tokio::test(flavor = "multi_thread")]
async fn test_rpc_schemas_includes_all_web_methods() {
    let (srv, _bg) = start_action_server().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut framed = connect(&srv.socket_path).await;

    // Authenticate
    send_frame(&mut framed, format!("HELLO {}", srv.token.0).as_bytes()).await;

    // Request rpc.schemas
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "rpc.schemas",
        "params": {}
    });
    send_frame(&mut framed, request.to_string().as_bytes()).await;

    let resp_bytes = recv_frame(&mut framed).await;
    let resp: serde_json::Value =
        serde_json::from_slice(&resp_bytes).expect("rpc.schemas response must be valid JSON");

    let methods = resp
        .get("result")
        .and_then(|r| r.get("methods"))
        .and_then(|m| m.as_array())
        .expect("rpc.schemas must return result.methods array");

    let method_names: Vec<&str> = methods
        .iter()
        .filter_map(|m| m.get("method").and_then(|v| v.as_str()))
        .collect();

    let expected_web_methods = [
        "web.click",
        "web.evaluate",
        "web.hover",
        "web.navigate",
        "web.screenshot",
        "web.scroll",
        "web.select",
        "web.snapshot",
        // Canonical name (was `web.type_text`); `web.type_text` is now an
        // alias surfaced via `MethodSchema::aliases`, not as a separate row.
        "web.type",
        "web.wait",
        "web.wait_for",
    ];

    for expected in &expected_web_methods {
        assert!(
            method_names.contains(expected),
            "rpc.schemas must include {expected}; got methods: {method_names:?}"
        );
    }

    let web_count = method_names
        .iter()
        .filter(|m| m.starts_with("web."))
        .count();
    assert_eq!(
        web_count, 11,
        "rpc.schemas must include exactly 11 web.* methods; got {web_count}: {method_names:?}"
    );
}

// ─── Test 3: web.navigate with "session" field (postinstall compat) ───────────

/// Verify parse_action accepts "session" field as alias for "session_id"
/// (postinstall schema compatibility).
#[tokio::test(flavor = "multi_thread")]
async fn test_web_navigate_accepts_session_field_alias() {
    let (srv, _bg) = start_action_server().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut framed = connect(&srv.socket_path).await;
    send_frame(&mut framed, format!("HELLO {}", srv.token.0).as_bytes()).await;

    // Use "session" instead of "session_id" (postinstall schema style)
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "web.navigate",
        "params": {
            "session": "test-session-alias",
            "url": "https://alias-test.example.com"
        }
    });
    send_frame(&mut framed, request.to_string().as_bytes()).await;

    let resp_bytes = recv_frame(&mut framed).await;
    let resp: serde_json::Value = serde_json::from_slice(&resp_bytes).unwrap();

    let result = resp
        .get("result")
        .expect("web.navigate with 'session' field must return a result (not error)");
    assert_eq!(
        result.get("action_id").and_then(|v| v.as_u64()),
        Some(42),
        "expected canned action_id=42; got: {result}"
    );
}

// ─── Test 4: — rpc.schemas advertises aliases ─────────────────

/// rpc.schemas reports the canonical method (`web.type`)
/// AND advertises `web.type_text` in the canonical's `aliases` array.
/// Without this, downstream tools that enumerate verbs from rpc.schemas
/// never discover the alias and break at runtime.
#[tokio::test(flavor = "multi_thread")]
async fn rpc_schemas_advertises_alias_for_web_type() {
    let (srv, _bg) = start_action_server().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut framed = connect(&srv.socket_path).await;
    send_frame(&mut framed, format!("HELLO {}", srv.token.0).as_bytes()).await;

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "rpc.schemas",
        "params": {}
    });
    send_frame(&mut framed, request.to_string().as_bytes()).await;

    let resp_bytes = recv_frame(&mut framed).await;
    let resp: serde_json::Value =
        serde_json::from_slice(&resp_bytes).expect("rpc.schemas response must be valid JSON");
    let methods = resp
        .get("result")
        .and_then(|r| r.get("methods"))
        .and_then(|m| m.as_array())
        .expect("rpc.schemas must return result.methods array");

    let web_type = methods
        .iter()
        .find(|m| m.get("method").and_then(|v| v.as_str()) == Some("web.type"))
        .expect("rpc.schemas must list canonical method 'web.type'");

    let aliases = web_type
        .get("aliases")
        .and_then(|a| a.as_array())
        .expect("web.type entry must have an 'aliases' array");
    let alias_strs: Vec<&str> = aliases.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        alias_strs.contains(&"web.type_text"),
        "aliases for web.type must include 'web.type_text'; got: {alias_strs:?}"
    );
}

// ─── Test 5: — request_router accepts the alias ───────────────

/// Sending the legacy `web.type_text` over the wire must reach the
/// `Action::WebType` handler (no MethodNotFound). Defence-in-depth: even
/// though the CLI canonicalises before sending, third-party clients
/// (mcp_adapter, future SDKs) may still send the alias.
#[tokio::test(flavor = "multi_thread")]
async fn request_router_accepts_web_type_text_alias() {
    let (srv, _bg) = start_action_server().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut framed = connect(&srv.socket_path).await;
    send_frame(&mut framed, format!("HELLO {}", srv.token.0).as_bytes()).await;

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "web.type_text",
        "params": {
            "session": "test-session-alias",
            "selector": "input",
            "text": "x"
        }
    });
    send_frame(&mut framed, request.to_string().as_bytes()).await;

    let resp_bytes = recv_frame(&mut framed).await;
    let resp: serde_json::Value = serde_json::from_slice(&resp_bytes).unwrap();

    // Either a "result" (success path) or no "method not found" error.
    if let Some(error) = resp.get("error") {
        let msg = error.get("message").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            !msg.contains("method not found"),
            "web.type_text must not produce 'method not found'; got: {msg}"
        );
    }
    // The canned host bridge returns action_id=42 for ALL action_dispatch
    // calls; if the alias resolved correctly, we get a successful result.
    assert!(
        resp.get("result").is_some(),
        "web.type_text alias must reach action_dispatch (got error: {resp})"
    );
}

// ─── Test 6: SDK `action.web.wait_for` alias routes end-to-end ────────────────

/// Both SDKs spell the settle-capture verb `action.web.wait_for`
/// (python `Session.wait_for`, typescript `Session.waitFor`) with the
/// SDK envelope params shape. A missing METHOD_ALIASES row made every
/// real-daemon call fail with `method_not_found` — the SDK test suites
/// masked it by registering mock handlers under the literal alias
/// string. Drive the exact SDK wire shape through canonicalise →
/// envelope unwrap → schema validation → router → action_dispatch.
#[tokio::test(flavor = "multi_thread")]
async fn request_router_accepts_sdk_wait_for_alias() {
    let (srv, _bg) = start_action_server().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut framed = connect(&srv.socket_path).await;
    send_frame(&mut framed, format!("HELLO {}", srv.token.0).as_bytes()).await;

    // Mirror python-sdk `_build_action_params(session_id, "wait_for",
    // {"until": "settled"}, deadline_ms)`: payload travels as a JSON
    // byte array inside the envelope.
    let payload: Vec<u8> = serde_json::json!({"until": "settled"})
        .to_string()
        .into_bytes();
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 6,
        "method": "action.web.wait_for",
        "params": {
            "session_id": "test-session-wait-for",
            "action": {
                "kind": "wait_for",
                "payload": payload,
                "deadline_ms": 30000
            }
        }
    });
    send_frame(&mut framed, request.to_string().as_bytes()).await;

    let resp_bytes = recv_frame(&mut framed).await;
    let resp: serde_json::Value = serde_json::from_slice(&resp_bytes).unwrap();

    if let Some(error) = resp.get("error") {
        let msg = error.get("message").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            !msg.contains("method not found"),
            "action.web.wait_for must not produce 'method not found'; got: {msg}"
        );
    }
    let result = resp.get("result").unwrap_or_else(|| {
        panic!("action.web.wait_for must reach action_dispatch (got error: {resp})")
    });
    assert_eq!(
        result.get("action_id").and_then(|v| v.as_u64()),
        Some(42),
        "expected canned action_id=42 from CannedHostBridge; got: {result}"
    );
}
