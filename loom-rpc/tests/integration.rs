//! Integration tests — `loom-rpc` contract.
//!
//! Contract: `projects/loom/architecture/contracts/loom-rpc_contract.md`
//!
//! These tests exercise the full socket server stack end-to-end:
//! framing → auth handshake → schema validation → dispatch → response.
//! No mocks; real SocketServer bound to a temp socket path per the contract.

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use loom_rpc::{
    auth_middleware::auth_middleware::{AuthMiddleware, Token},
    connection_handler::connection_handler::ConnectionHandlerDeps,
    frame_handler::frame_handler::FrameHandler,
    request_router::request_router::RequestRouter,
    rpc_handlers::rpc_handlers::RpcHandlers,
    rpc_observability::rpc_observability::RpcObservability,
    schema_validator::schema_validator::SchemaValidator,
    socket_server::socket_server::{SocketServer, SocketServerConfig},
};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::net::UnixStream;

mod common;

// ─── Test infrastructure ─────────────────────────────────────────────────────

/// Minimal SchemaProvider that returns empty registry (no schemas = every
/// request passes validation, which is correct for testing the RPC layer).
mod stubs {
    use loom_rpc::schema_provider::schema_provider::{
        CompiledJsonSchema, SchemaProviderApi, SchemaRegistry,
    };
    use std::sync::Arc;

    pub struct EmptySchemas;

    impl SchemaProviderApi for EmptySchemas {
        fn lookup_request_schema(&self, _method: &str) -> Option<Arc<CompiledJsonSchema>> {
            None
        }
        fn lookup_response_schema(&self, _method: &str) -> Option<Arc<CompiledJsonSchema>> {
            None
        }
        fn registered_methods(&self) -> Vec<String> {
            // rpc.schemas is a built-in with no schema definition; its presence
            // in registered_methods tells the validator to pass it without
            // param validation.
            vec!["rpc.schemas".to_string()]
        }
        fn get_registry_snapshot(&self) -> SchemaRegistry {
            SchemaRegistry {
                methods: vec![],
                source_wit_sha256: String::new(),
            }
        }
    }
}

struct TestServer {
    _dir: TempDir,
    token: Token,
    socket_path: std::path::PathBuf,
}

/// Spin up a real SocketServer; return the test handle and a background join handle.
async fn start_server() -> (TestServer, tokio::task::JoinHandle<()>) {
    // Hold the bind lock across TempDir creation through the bind: `try_bind`'s process-global
    // umask would otherwise corrupt a concurrent test's `TempDir::new()` (see common::BIND_LOCK).
    // Dropped before the first `.await` below, so the future stays Send.
    let bind_lock = common::bind_guard();
    let dir = TempDir::new().unwrap();
    let socket_path = dir.path().join("test.sock");

    let token = Token::generate();
    let token_arc = Arc::new(token.clone());

    let schemas: Arc<dyn loom_rpc::schema_provider::schema_provider::SchemaProviderApi> =
        Arc::new(stubs::EmptySchemas);
    let validator: Arc<dyn loom_rpc::schema_validator::schema_validator::SchemaValidatorApi> =
        SchemaValidator::new(Arc::clone(&schemas));
    let obs: Arc<dyn loom_rpc::rpc_observability::rpc_observability::RpcObservabilityApi> =
        RpcObservability::new();
    let auth: Arc<dyn loom_rpc::auth_middleware::auth_middleware::AuthMiddlewareApi> =
        AuthMiddleware::new(Arc::clone(&token_arc));

    // Stub CoreServiceAdapter using the empty-schemas path
    let core_bridge = Arc::new(NoopCoreBridge);
    let core_adapter =
        loom_rpc::core_service_adapter::core_service_adapter::CoreServiceAdapter::new(
            core_bridge
                as Arc<dyn loom_rpc::core_service_adapter::core_service_adapter::CoreFacadeBridge>,
        );
    let host_adapter =
        loom_rpc::host_service_adapter::host_service_adapter::HostServiceAdapter::new(Arc::new(
            StubHostBridge,
        )
            as Arc<dyn loom_rpc::host_service_adapter::host_service_adapter::WasmHostBridge>);

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
        TestServer {
            _dir: dir,
            token,
            socket_path,
        },
        join,
    )
}

/// Send a length-delimited frame to the server.
async fn send_frame(
    framed: &mut tokio_util::codec::Framed<UnixStream, tokio_util::codec::LengthDelimitedCodec>,
    data: &[u8],
) {
    framed.send(Bytes::copy_from_slice(data)).await.unwrap();
}

/// Read the next frame from the server.
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

/// Connect to the server and wrap in a LengthDelimitedCodec framed stream.
async fn connect(
    socket_path: &std::path::Path,
) -> tokio_util::codec::Framed<UnixStream, tokio_util::codec::LengthDelimitedCodec> {
    let stream = UnixStream::connect(socket_path)
        .await
        .expect("connect must succeed");
    FrameHandler::wrap_stream(stream)
}

// Stub bridges for tests that don't need core functionality
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

struct StubHostBridge;
impl loom_rpc::host_service_adapter::host_service_adapter::WasmHostBridge for StubHostBridge {
    fn dispatch_action_blocking(
        &self,
        _action: loom_rpc::host_service_adapter::host_service_adapter::Action,
        _deadline_ms: Option<u64>,
    ) -> Result<
        loom_rpc::host_service_adapter::host_service_adapter::Receipt,
        loom_rpc::host_service_adapter::host_service_adapter::AdapterError,
    > {
        Err(loom_rpc::error_translator::error_translator::LoomErrorCode::SurfaceUnavailable)
    }
}

// ─── Test 1: Happy path — HELLO + rpc.schemas returns registry ───────────────

/// Contract: — first message MUST be `HELLO {token}`.
/// Contract: — `rpc.schemas()` returns the in-memory schema registry.
///
/// This test validates the full happy path: bind socket → connect → HELLO token →
/// send `rpc.schemas` request → receive JSON response with method registry.
#[tokio::test]
async fn test_hello_auth_and_rpc_schemas() {
    let (srv, _bg) = start_server().await;

    // Allow server to start
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut framed = connect(&srv.socket_path).await;

    // Step 1: send valid HELLO token
    let hello = format!("HELLO {}", srv.token.0);
    send_frame(&mut framed, hello.as_bytes()).await;

    // Step 2: request rpc.schemas
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "rpc.schemas",
        "params": {}
    });
    send_frame(&mut framed, request.to_string().as_bytes()).await;

    // Step 3: receive response — must be valid JSON with methods field
    let resp_bytes = recv_frame(&mut framed).await;
    let resp: serde_json::Value =
        serde_json::from_slice(&resp_bytes).expect("rpc.schemas response must be valid JSON");

    // Response must be a JSON-RPC 2.0 success envelope with "result.methods"
    assert!(
        resp.get("result").and_then(|r| r.get("methods")).is_some(),
        "rpc.schemas response must contain 'result.methods' field; got: {resp}"
    );
}

// ─── Test 2: Error path — wrong HELLO token closes connection ────────────────

/// Contract: — token mismatch → `code = "protocol_auth_required"`.
///
/// Validates that the server rejects a connection with a wrong HELLO token
/// and returns the typed auth error envelope before closing.
#[tokio::test]
async fn test_wrong_hello_token_returns_auth_error() {
    let (srv, _bg) = start_server().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut framed = connect(&srv.socket_path).await;

    // Send wrong token
    let bad_hello = "HELLO baad0000baad0000baad0000baad0000baad0000baad0000baad0000baad0000";
    send_frame(&mut framed, bad_hello.as_bytes()).await;

    // Server should respond with auth error then close
    let resp_bytes = recv_frame(&mut framed).await;
    let resp: serde_json::Value =
        serde_json::from_slice(&resp_bytes).expect("auth error response must be valid JSON");

    // Must include code = "protocol_auth_required"
    let code = resp.get("code").and_then(|v| v.as_str()).unwrap_or("");
    assert_eq!(
        code, "protocol_auth_required",
        "wrong token must return protocol_auth_required; got: {resp}"
    );
}

// ─── Test 3: Error path — malformed HELLO (no space) ─────────────────────────

/// Contract: — first frame must be `HELLO {token}` (exactly).
/// A malformed frame (no space, wrong prefix) must close with auth error.
#[tokio::test]
async fn test_malformed_hello_frame_returns_auth_error() {
    let (srv, _bg) = start_server().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut framed = connect(&srv.socket_path).await;

    // Send a malformed HELLO (missing space)
    send_frame(&mut framed, b"HELO_WRONG").await;

    let resp_bytes = recv_frame(&mut framed).await;
    let resp: serde_json::Value =
        serde_json::from_slice(&resp_bytes).expect("error response must be valid JSON");

    let code = resp.get("code").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        code == "protocol_auth_required",
        "malformed HELLO must return auth error; got code={code}, resp={resp}"
    );
}

// ─── Test 4: Error path — malformed JSON after auth ──────────────────────────

/// Contract: — invalid JSON → `code = "protocol_malformed"`.
/// After a valid HELLO, sending non-JSON must return ProtocolMalformed.
#[tokio::test]
async fn test_authenticated_malformed_json_returns_protocol_error() {
    let (srv, _bg) = start_server().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut framed = connect(&srv.socket_path).await;

    // Authenticate successfully
    send_frame(&mut framed, format!("HELLO {}", srv.token.0).as_bytes()).await;

    // Send garbage bytes (not JSON)
    send_frame(&mut framed, b"this is not json!!!").await;

    let resp_bytes = recv_frame(&mut framed).await;
    let resp: serde_json::Value =
        serde_json::from_slice(&resp_bytes).expect("error response must be valid JSON");

    let code = resp
        .get("error")
        .and_then(|e| e.get("code"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(
        code, "protocol_malformed",
        "non-JSON request must return protocol_malformed; got: {resp}"
    );
}

// ─── Test 5: Concurrent connections are independent ──────────────────────────

/// Contract: — each connection runs on its own tokio task.
/// Two simultaneous connections with different tokens should each get
/// their respective responses independently.
#[tokio::test]
async fn test_concurrent_connections_are_independent() {
    let (srv, _bg) = start_server().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Two connections: conn1 sends valid HELLO, conn2 sends bad HELLO
    let (mut conn1, mut conn2) = tokio::join!(connect(&srv.socket_path), connect(&srv.socket_path));

    // conn1: valid HELLO
    send_frame(&mut conn1, format!("HELLO {}", srv.token.0).as_bytes()).await;
    // conn2: invalid HELLO
    send_frame(&mut conn2, b"HELLO definitely_wrong_token_that_will_fail").await;

    // conn2 should get auth error
    let resp2 = recv_frame(&mut conn2).await;
    let v2: serde_json::Value = serde_json::from_slice(&resp2).unwrap();
    assert_eq!(
        v2.get("code").and_then(|c| c.as_str()),
        Some("protocol_auth_required")
    );

    // conn1 can still dispatch (send rpc.schemas)
    send_frame(
        &mut conn1,
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"rpc.schemas","params":{}})
            .to_string()
            .as_bytes(),
    )
    .await;
    let resp1 = recv_frame(&mut conn1).await;
    let v1: serde_json::Value = serde_json::from_slice(&resp1).unwrap();
    assert!(
        v1.get("result").and_then(|r| r.get("methods")).is_some(),
        "conn1 must still work after conn2 fails; got: {v1}"
    );
}

// ─── Test 6: content.get rejects malformed artifact_refs without aborting ────

/// Regression for the daemon-abort panic: `content.get` forwarded
/// `artifact_ref` straight into the content store's `shard_path`, which
/// byte-sliced refs shorter than the shard prefix out of bounds — with
/// `panic = "abort"` that killed the whole daemon. Malformed refs must now
/// come back as a typed `schema_violation` envelope on a live connection.
#[tokio::test]
async fn test_content_get_malformed_artifact_ref_returns_schema_violation() {
    let (srv, _bg) = start_server().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut framed = connect(&srv.socket_path).await;
    send_frame(&mut framed, format!("HELLO {}", srv.token.0).as_bytes()).await;

    let bad_refs: Vec<serde_json::Value> = vec![
        serde_json::json!(""),
        serde_json::json!("a"),
        serde_json::json!("abc"),
        // 64 chars but not lowercase hex.
        serde_json::json!("Z".repeat(64)),
        // Over-long.
        serde_json::json!("a".repeat(100)),
        // Omitted param defaults to "" in the router.
        serde_json::Value::Null,
    ];
    for (i, bad) in bad_refs.into_iter().enumerate() {
        let params = match &bad {
            serde_json::Value::Null => serde_json::json!({}),
            v => serde_json::json!({ "artifact_ref": v }),
        };
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": i + 1,
            "method": "content.get",
            "params": params,
        });
        send_frame(&mut framed, request.to_string().as_bytes()).await;

        let resp_bytes = recv_frame(&mut framed).await;
        let resp: serde_json::Value =
            serde_json::from_slice(&resp_bytes).expect("daemon must answer, not abort");
        let code = resp
            .get("error")
            .and_then(|e| e.get("code"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(
            code, "schema_violation",
            "malformed ref {bad:?} must be rejected as schema_violation; got: {resp}"
        );
    }

    // A well-formed (64-char lowercase hex) ref passes validation and
    // reaches the core bridge — the stub returns internal_error, proving
    // the guard doesn't over-block plausible refs.
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 99,
        "method": "content.get",
        "params": { "artifact_ref": "a".repeat(64) },
    });
    send_frame(&mut framed, request.to_string().as_bytes()).await;
    let resp_bytes = recv_frame(&mut framed).await;
    let resp: serde_json::Value = serde_json::from_slice(&resp_bytes).unwrap();
    let code = resp
        .get("error")
        .and_then(|e| e.get("code"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(
        code, "internal_error",
        "well-formed ref must reach the core bridge; got: {resp}"
    );
}

// ─── Test 7: Oversized frame → typed error envelope, then close ──────────────

/// Regression: a frame whose length prefix exceeds `MAX_FRAME_BYTES`
/// (e.g. `loom import playwright` hex-encoding a >8 MiB trace into one
/// JSON-RPC frame) used to be silently dropped by the authenticated
/// loop's `_ => break` arm — the client saw a bare connection close and
/// surfaced it as "no daemon running". The handler must now send a
/// `protocol_malformed` JSON-RPC error envelope naming the frame cap,
/// then close the connection (LengthDelimitedCodec cannot resync after
/// an oversized length prefix, so the close itself is unavoidable).
#[tokio::test]
async fn test_oversized_frame_returns_protocol_malformed_then_closes() {
    use loom_rpc::frame_handler::frame_handler::MAX_FRAME_BYTES;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (srv, _bg) = start_server().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Raw stream — the test client's own LengthDelimitedCodec would
    // refuse to *encode* an oversized frame, so write the length prefix
    // by hand. The server decoder rejects on the prefix alone; no
    // payload bytes are required.
    let mut stream = UnixStream::connect(&srv.socket_path)
        .await
        .expect("connect must succeed");

    // Authenticate (manual 4-byte big-endian length framing).
    let hello = format!("HELLO {}", srv.token.0);
    stream
        .write_all(&(hello.len() as u32).to_be_bytes())
        .await
        .unwrap();
    stream.write_all(hello.as_bytes()).await.unwrap();

    // Claim a frame one byte over the cap.
    stream
        .write_all(&((MAX_FRAME_BYTES + 1) as u32).to_be_bytes())
        .await
        .unwrap();

    // The server must answer with a typed error envelope, not a bare close.
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .expect("server must send an error frame before closing");
    let mut buf = vec![0u8; u32::from_be_bytes(len_buf) as usize];
    stream.read_exact(&mut buf).await.unwrap();
    let resp: serde_json::Value =
        serde_json::from_slice(&buf).expect("error frame must be valid JSON");
    assert_eq!(
        resp.get("error")
            .and_then(|e| e.get("code"))
            .and_then(|c| c.as_str()),
        Some("protocol_malformed"),
        "oversized frame must yield protocol_malformed; got: {resp}"
    );
    let message = resp["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains(&MAX_FRAME_BYTES.to_string()),
        "error must name the frame cap; got: {message}"
    );

    // ... and then the connection closes (codec cannot resync).
    let n = stream.read(&mut [0u8; 16]).await.unwrap_or(0);
    assert_eq!(n, 0, "connection must be closed after the error frame");
}
