//! Wire-boundary integration tests for `vault.*` JSON-RPC methods.
//!
//! Asserts the full `request_router → rpc_handlers → CoreServiceAdapter →
//! CoreFacadeBridge` chain returns the contract-shaped envelopes for:
//!
//! - **AC-VAULTRPC-01** `vault.list_grants` returns a (possibly empty) JSON array.
//! - **AC-VAULTRPC-02** `vault.add` returns the typed receipt for allowlisted
//!   providers AND emits the canonical `oauth-only` rejection envelope for
//!   non-allowlisted providers (per AC-VAULT-04.1).
//! - **AC-VAULTRPC-03** `vault.grant` returns `GrantInfo` with `grant_id` only
//!   (IC-RPC-10 — never the secret bytes).
//! - **AC-VAULTRPC-04** `vault.revoke` succeeds; subsequent revoke surfaces the
//!   distinct `vault_grant_revoked` wire kind (not collapsed to
//!   `vault_grant_not_found`).
//!
//! Plus an AC-CLI-01.1 spot-check on canonical JCS response encoding.
//!
//! Uses `EmptySchemaProvider` (validation bypassed when no methods are
//! registered) and a `RecordingCoreBridge` that returns canned values.

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use loom_rpc::{
    auth_middleware::auth_middleware::{AuthMiddleware, Token},
    connection_handler::connection_handler::ConnectionHandlerDeps,
    core_service_adapter::core_service_adapter::{
        CoreFacadeBridge, CoreServiceAdapter, GrantInfo, GrantParams, VaultAddInfo, VaultAddParams,
    },
    error_translator::error_translator::LoomErrorCode,
    frame_handler::frame_handler::FrameHandler,
    host_service_adapter::host_service_adapter::{
        Action, HostServiceAdapter, Receipt, ReceiptStatus, WasmHostBridge,
    },
    request_router::request_router::RequestRouter,
    rpc_handlers::rpc_handlers::RpcHandlers,
    rpc_observability::rpc_observability::RpcObservability,
    schema_validator::schema_validator::SchemaValidator,
    socket_server::socket_server::{SocketServer, SocketServerConfig},
};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;
use tokio::net::UnixStream;

mod common;
use common::EmptySchemaProvider;

// ─── RecordingCoreBridge ─────────────────────────────────────────────────────

#[derive(Default)]
struct VaultCalls {
    grant: Vec<GrantParams>,
    revoke: Vec<(String, String)>,
    list_grants: Vec<Option<String>>,
    add: Vec<VaultAddParams>,
    /// Tracks which grant_ids have been revoked so the second revoke
    /// returns `VaultGrantRevoked` (AC-VAULTRPC-04 distinct kind).
    revoked_ids: std::collections::HashSet<String>,
}

struct RecordingCoreBridge {
    calls: Mutex<VaultCalls>,
}

impl RecordingCoreBridge {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(VaultCalls::default()),
        })
    }
}

impl CoreFacadeBridge for RecordingCoreBridge {
    fn export_session_to_cas(
        &self,
        _: &str,
        _: &str,
    ) -> Result<loom_rpc::core_service_adapter::core_service_adapter::ExportInfo, LoomErrorCode>
    {
        Err(LoomErrorCode::InternalError)
    }
    fn get_export_bytes(&self, _: &str) -> Result<Vec<u8>, LoomErrorCode> {
        Err(LoomErrorCode::InternalError)
    }
    fn list_sessions_info(&self) -> Result<Vec<(String, String, u64)>, LoomErrorCode> {
        Ok(vec![])
    }
    fn replay_session_to_id(&self, _: &str) -> Result<String, LoomErrorCode> {
        Err(LoomErrorCode::InternalError)
    }
    fn diff_sessions_json(
        &self,
        _: &str,
        _: &str,
        _: bool,
    ) -> Result<serde_json::Value, LoomErrorCode> {
        Err(LoomErrorCode::InternalError)
    }
    fn inspect_session_json(
        &self,
        _: &str,
        _: Option<u64>,
    ) -> Result<serde_json::Value, LoomErrorCode> {
        Err(LoomErrorCode::InternalError)
    }
    fn validate_session_result(&self, _: &str) -> Result<(bool, Vec<String>), LoomErrorCode> {
        Err(LoomErrorCode::InternalError)
    }
    fn import_playwright_from_bytes(
        &self,
        _: &[u8],
    ) -> Result<
        loom_rpc::core_service_adapter::core_service_adapter::PlaywrightImportInfo,
        LoomErrorCode,
    > {
        Err(LoomErrorCode::InternalError)
    }
    fn create_session_raw(
        &self,
        _: &str,
        _: &str,
        _: Option<&str>,
        _: Option<u64>,
        _: Option<serde_json::Value>,
        _: bool,
    ) -> Result<(String, u64), LoomErrorCode> {
        Err(LoomErrorCode::InternalError)
    }
    fn close_session_raw(&self, _: &str) -> Result<(), LoomErrorCode> {
        Err(LoomErrorCode::InternalError)
    }
    fn abort_session_raw(&self, _: &str, _: &str) -> Result<(), LoomErrorCode> {
        Err(LoomErrorCode::InternalError)
    }

    fn vault_grant(&self, p: GrantParams) -> Result<GrantInfo, LoomErrorCode> {
        let info = GrantInfo {
            grant_id: "01HCANNED-GRANT-ID".to_string(),
            origin: p.origin.clone(),
            scopes: p.scopes.clone(),
            ttl_seconds: p.ttl_seconds,
            label: p.label.clone(),
        };
        self.calls.lock().unwrap().grant.push(p);
        Ok(info)
    }

    fn vault_revoke(&self, grant_id: &str, reason: &str) -> Result<(), LoomErrorCode> {
        let mut calls = self.calls.lock().unwrap();
        calls
            .revoke
            .push((grant_id.to_string(), reason.to_string()));
        if !calls.revoked_ids.insert(grant_id.to_string()) {
            // Second revoke of the same grant_id → distinct VaultGrantRevoked
            // wire kind per AC-VAULTRPC-04.
            return Err(LoomErrorCode::VaultGrantRevoked);
        }
        Ok(())
    }

    fn vault_list_grants(&self, session_id: Option<&str>) -> Result<Vec<GrantInfo>, LoomErrorCode> {
        self.calls
            .lock()
            .unwrap()
            .list_grants
            .push(session_id.map(String::from));
        Ok(vec![])
    }

    fn vault_add(&self, p: VaultAddParams) -> Result<VaultAddInfo, LoomErrorCode> {
        // Mirror the loom-core allowlist semantics: github → typed receipt;
        // anything else → VaultRejection envelope.
        if p.provider != "github" {
            self.calls.lock().unwrap().add.push(p);
            return Err(LoomErrorCode::VaultRejection);
        }
        let label = p
            .label
            .clone()
            .unwrap_or_else(|| format!("{}/oauth_token", p.provider));
        let info = VaultAddInfo {
            provider: p.provider.clone(),
            label,
            status: "oauth_required".to_string(),
        };
        self.calls.lock().unwrap().add.push(p);
        Ok(info)
    }
    fn gc_run(
        &self,
        _: Option<u64>,
        _: Option<u64>,
    ) -> Result<loom_rpc::core_service_adapter::core_service_adapter::GcRunReport, LoomErrorCode>
    {
        Ok(loom_rpc::core_service_adapter::core_service_adapter::GcRunReport::default())
    }
}

// ─── NoopHostBridge (action.* not exercised here) ────────────────────────────

struct NoopHostBridge;
impl WasmHostBridge for NoopHostBridge {
    fn dispatch_action_blocking(
        &self,
        _: Action,
    ) -> Result<Receipt, loom_rpc::host_service_adapter::host_service_adapter::AdapterError> {
        Ok(Receipt {
            action_id: 0,
            session_id: String::new(),
            status: ReceiptStatus::Success,
            timing_ticks: 0,
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
            screenshot_after_hash: None,
            console_count: None,
            network_count: None,
            console_lines: vec![],
            network_summary: None,
            return_value_json: None,
            return_value_blob_ref: None,
        })
    }
}

// ─── Test server ─────────────────────────────────────────────────────────────

struct VaultTestServer {
    _dir: TempDir,
    token: Token,
    socket_path: std::path::PathBuf,
}

async fn start_vault_server() -> (VaultTestServer, tokio::task::JoinHandle<()>) {
    let dir = TempDir::new().unwrap();
    let socket_path = dir.path().join("vault_test.sock");
    let token = Token::generate();
    let token_arc = Arc::new(token.clone());

    let schemas: Arc<dyn loom_rpc::schema_provider::schema_provider::SchemaProviderApi> =
        Arc::new(EmptySchemaProvider);
    let validator: Arc<dyn loom_rpc::schema_validator::schema_validator::SchemaValidatorApi> =
        SchemaValidator::new(Arc::clone(&schemas));
    let obs: Arc<dyn loom_rpc::rpc_observability::rpc_observability::RpcObservabilityApi> =
        RpcObservability::new();
    let auth: Arc<dyn loom_rpc::auth_middleware::auth_middleware::AuthMiddlewareApi> =
        AuthMiddleware::new(Arc::clone(&token_arc));

    let core_bridge: Arc<dyn CoreFacadeBridge> = RecordingCoreBridge::new();
    let core_adapter = CoreServiceAdapter::new(Arc::clone(&core_bridge));
    let host_adapter = HostServiceAdapter::new(Arc::new(NoopHostBridge) as Arc<dyn WasmHostBridge>);

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
    let handle = tokio::runtime::Handle::current();
    let join = tokio::spawn(async move {
        server.serve(handle, futures::future::pending::<()>()).await;
    });

    (
        VaultTestServer {
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

async fn rpc_call(
    framed: &mut tokio_util::codec::Framed<UnixStream, tokio_util::codec::LengthDelimitedCodec>,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    send_frame(framed, req.to_string().as_bytes()).await;
    let bytes = recv_frame(framed).await;
    serde_json::from_slice(&bytes).expect("response must be valid JSON")
}

async fn authenticate(
    framed: &mut tokio_util::codec::Framed<UnixStream, tokio_util::codec::LengthDelimitedCodec>,
    token: &Token,
) {
    send_frame(framed, format!("HELLO {}", token.0).as_bytes()).await;
}

// ─── Tests ───────────────────────────────────────────────────────────────────

/// AC-VAULTRPC-01: `vault.list_grants` returns a (possibly empty) JSON array.
#[tokio::test(flavor = "multi_thread")]
async fn vault_list_returns_empty_array() {
    let (srv, _bg) = start_vault_server().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let mut framed = connect(&srv.socket_path).await;
    authenticate(&mut framed, &srv.token).await;

    let resp = rpc_call(&mut framed, 1, "vault.list_grants", serde_json::json!({})).await;

    let result = resp
        .get("result")
        .expect("vault.list_grants must return result");
    let arr = result.as_array().expect("result must be a JSON array");
    assert!(arr.is_empty(), "expected empty array; got: {result}");
}

/// AC-VAULTRPC-02 accept branch: `vault.add github` returns typed receipt.
#[tokio::test(flavor = "multi_thread")]
async fn vault_add_github_returns_typed_receipt() {
    let (srv, _bg) = start_vault_server().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let mut framed = connect(&srv.socket_path).await;
    authenticate(&mut framed, &srv.token).await;

    let resp = rpc_call(
        &mut framed,
        1,
        "vault.add",
        serde_json::json!({ "provider": "github", "yes": true }),
    )
    .await;

    let result = resp.get("result").expect("vault.add must return result");
    assert_eq!(
        result.get("provider").and_then(|v| v.as_str()),
        Some("github")
    );
    assert_eq!(
        result.get("status").and_then(|v| v.as_str()),
        Some("oauth_required")
    );
    assert!(result.get("label").is_some(), "label field must be present");
}

/// AC-VAULTRPC-02 reject branch: non-allowlisted provider rejects with
/// the `vault_rejection` wire kind. AC-VAULT-04.1's structured `details`
/// payload is asserted in the loom-core unit tests; here we assert the
/// router emits a JSON-RPC error envelope (not a method-not-found prose
/// or a successful result).
#[tokio::test(flavor = "multi_thread")]
async fn vault_add_unknown_provider_rejects_oauth_only() {
    let (srv, _bg) = start_vault_server().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let mut framed = connect(&srv.socket_path).await;
    authenticate(&mut framed, &srv.token).await;

    let resp = rpc_call(
        &mut framed,
        1,
        "vault.add",
        serde_json::json!({ "provider": "unknown-provider", "yes": true }),
    )
    .await;

    assert!(
        resp.get("result").is_none(),
        "non-allowlisted must NOT return result"
    );
    let err = resp
        .get("error")
        .expect("vault.add for unknown provider must return error");
    let code = err.get("code").and_then(|v| v.as_str()).unwrap_or("");
    assert_eq!(
        code, "vault_rejection",
        "unknown-provider must produce vault_rejection wire kind; got: {err}"
    );
}

/// AC-VAULTRPC-03 + IC-RPC-10: `vault.grant` returns GrantInfo with grant_id only.
#[tokio::test(flavor = "multi_thread")]
async fn vault_grant_returns_grant_info_with_grant_id_only() {
    let (srv, _bg) = start_vault_server().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let mut framed = connect(&srv.socket_path).await;
    authenticate(&mut framed, &srv.token).await;

    let resp = rpc_call(
        &mut framed,
        1,
        "vault.grant",
        serde_json::json!({
            "session_id": "sess-test-01",
            "origin": "https://example.com",
            "scopes": ["repo"],
            "ttl_seconds": 3600,
            "label": "test-label",
        }),
    )
    .await;

    let result = resp.get("result").expect("vault.grant must return result");
    assert!(
        result.get("grant_id").and_then(|v| v.as_str()).is_some(),
        "GrantInfo.grant_id must be present"
    );
    // IC-RPC-10: response carries no secret bytes. The struct field listing
    // (grant_id, origin, scopes, ttl_seconds, label) is exhaustive — the
    // serde-derived JSON has no `secret`/`token`/`value` fields.
    for forbidden in &["secret", "token", "value"] {
        assert!(
            result.get(forbidden).is_none(),
            "GrantInfo MUST NOT carry `{forbidden}`; got: {result}"
        );
    }
}

/// AC-VAULTRPC-04: `vault.revoke` succeeds; subsequent revoke of the same
/// grant_id surfaces the distinct `vault_grant_revoked` wire kind (NOT
/// collapsed to `vault_grant_not_found` — that was the F-A2 bug).
#[tokio::test(flavor = "multi_thread")]
async fn vault_revoke_succeeds_and_double_revoke_envelope_is_typed() {
    let (srv, _bg) = start_vault_server().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let mut framed = connect(&srv.socket_path).await;
    authenticate(&mut framed, &srv.token).await;

    // First revoke: succeeds.
    let resp1 = rpc_call(
        &mut framed,
        1,
        "vault.revoke",
        serde_json::json!({ "grant_id": "01HFAKEGRANT", "reason": "user-initiated" }),
    )
    .await;
    assert!(
        resp1.get("error").is_none(),
        "first revoke must NOT return error; got: {resp1}"
    );

    // Second revoke of same grant_id: distinct vault_grant_revoked kind.
    let resp2 = rpc_call(
        &mut framed,
        2,
        "vault.revoke",
        serde_json::json!({ "grant_id": "01HFAKEGRANT", "reason": "user-initiated" }),
    )
    .await;
    let err = resp2.get("error").expect("second revoke must return error");
    let code = err.get("code").and_then(|v| v.as_str()).unwrap_or("");
    assert_eq!(
        code, "vault_grant_revoked",
        "second revoke MUST surface distinct vault_grant_revoked kind \
         (not collapsed to vault_grant_not_found); got: {err}"
    );
}

/// AC-CLI-01.1 spot-check: the response payload encodes as canonical JCS.
/// Object keys serialise in sorted order (RFC 8785). We assert the
/// serialisation by reading the raw response bytes and re-parsing into a
/// `serde_json::Map` to verify field ordering.
#[tokio::test(flavor = "multi_thread")]
async fn vault_method_emits_canonical_jcs_response() {
    let (srv, _bg) = start_vault_server().await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let mut framed = connect(&srv.socket_path).await;
    authenticate(&mut framed, &srv.token).await;

    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "vault.add",
        "params": { "provider": "github", "yes": true },
    });
    send_frame(&mut framed, req.to_string().as_bytes()).await;
    let raw_bytes = recv_frame(&mut framed).await;

    let raw_str = std::str::from_utf8(&raw_bytes).expect("response must be valid UTF-8");
    // Locate the inner result object substring.
    let result_start = raw_str
        .find("\"result\"")
        .expect("response must contain \"result\" field");
    let after_colon = raw_str[result_start..].find(':').expect("malformed json");
    let obj_start = result_start + after_colon + 1;
    // Skip whitespace and find the opening brace.
    let trimmed = &raw_str[obj_start..].trim_start();
    assert!(
        trimmed.starts_with('{'),
        "result must serialise as JSON object; got prefix: {:?}",
        &trimmed[..20.min(trimmed.len())]
    );

    // Verify keys appear in JCS canonical order (alphabetical): label, provider, status.
    let label_idx = trimmed.find("\"label\"").expect("label field missing");
    let provider_idx = trimmed
        .find("\"provider\"")
        .expect("provider field missing");
    let status_idx = trimmed.find("\"status\"").expect("status field missing");
    assert!(
        label_idx < provider_idx,
        "JCS requires alphabetical key order; label must precede provider in: {trimmed}"
    );
    assert!(
        provider_idx < status_idx,
        "JCS requires alphabetical key order; provider must precede status in: {trimmed}"
    );
}
