//! Integration tests — AC-PROFVAL-01/02/03/04 (parent AC-PROTO-02.1).
//!
//! Verifies that `session.create` rejects unrecognized profile,
//! network-mode, and budget-key values with typed `JsonRpcError`
//! envelopes (`code` = `unknown_profile` / `invalid_network_mode` /
//! `invalid_budget_key`) carrying `data: {provided, available}` —
//! BEFORE any call reaches the core adapter.
//!
//! Hits the `RpcHandlers::session_create` surface directly (no socket)
//! using `NoopCoreBridge`: if validation skipped, the adapter's
//! `InternalError` would leak through, so a clean rejection envelope
//! proves the typed-validation path runs first.

use loom_rpc::{
    core_service_adapter::core_service_adapter::{
        CoreFacadeBridge, CoreServiceAdapter, CreateSessionParams, ExportInfo,
    },
    error_translator::error_translator::LoomErrorCode,
    host_service_adapter::host_service_adapter::{Action, HostServiceAdapter, Receipt, WasmHostBridge},
    rpc_handlers::rpc_handlers::RpcHandlers,
    rpc_observability::rpc_observability::RpcObservability,
    schema_provider::schema_provider::{CompiledJsonSchema, SchemaProviderApi, SchemaRegistry},
    schema_validator::schema_validator::SchemaValidator,
};
use std::sync::Arc;

// --- Stub bridges (mirrors `tests/integration.rs`'s NoopCoreBridge /
// StubHostBridge): every method returns InternalError so an invocation
// that bypasses validation surfaces visibly. ----------------------------

struct NoopCoreBridge;
impl CoreFacadeBridge for NoopCoreBridge {
    fn export_session_to_cas(&self, _: &str, _: &str) -> Result<ExportInfo, LoomErrorCode> {
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
    ) -> Result<loom_rpc::core_service_adapter::core_service_adapter::PlaywrightImportInfo, LoomErrorCode>
    {
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
    fn vault_grant(
        &self,
        _: loom_rpc::core_service_adapter::core_service_adapter::GrantParams,
    ) -> Result<loom_rpc::core_service_adapter::core_service_adapter::GrantInfo, LoomErrorCode> {
        Err(LoomErrorCode::InternalError)
    }
    fn vault_revoke(&self, _: &str, _: &str) -> Result<(), LoomErrorCode> {
        Err(LoomErrorCode::InternalError)
    }
    fn vault_list_grants(
        &self,
        _: Option<&str>,
    ) -> Result<Vec<loom_rpc::core_service_adapter::core_service_adapter::GrantInfo>, LoomErrorCode> {
        Ok(vec![])
    }
    fn vault_add(
        &self,
        _: loom_rpc::core_service_adapter::core_service_adapter::VaultAddParams,
    ) -> Result<loom_rpc::core_service_adapter::core_service_adapter::VaultAddInfo, LoomErrorCode> {
        Err(LoomErrorCode::InternalError)
    }
    fn gc_run(
        &self,
        _: Option<u64>,
        _: Option<u64>,
    ) -> Result<loom_rpc::core_service_adapter::core_service_adapter::GcRunReport, LoomErrorCode> {
        Ok(loom_rpc::core_service_adapter::core_service_adapter::GcRunReport::default())
    }
}

struct StubHostBridge;
impl WasmHostBridge for StubHostBridge {
    fn dispatch_action_blocking(&self, _: Action) -> Result<Receipt, LoomErrorCode> {
        Err(LoomErrorCode::SurfaceUnavailable)
    }
}

struct EmptySchemas;
impl SchemaProviderApi for EmptySchemas {
    fn lookup_request_schema(&self, _: &str) -> Option<Arc<CompiledJsonSchema>> {
        None
    }
    fn lookup_response_schema(&self, _: &str) -> Option<Arc<CompiledJsonSchema>> {
        None
    }
    fn registered_methods(&self) -> Vec<String> {
        vec!["rpc.schemas".to_string()]
    }
    fn get_registry_snapshot(&self) -> SchemaRegistry {
        SchemaRegistry {
            methods: vec![],
            source_wit_sha256: String::new(),
        }
    }
}

fn make_handlers() -> Arc<RpcHandlers> {
    let schemas: Arc<dyn SchemaProviderApi> = Arc::new(EmptySchemas);
    let validator = SchemaValidator::new(Arc::clone(&schemas));
    let obs = RpcObservability::new();
    let core = CoreServiceAdapter::new(Arc::new(NoopCoreBridge));
    let host = HostServiceAdapter::new(Arc::new(StubHostBridge));
    RpcHandlers::new(core, host, schemas, validator, obs)
}

fn create_params(
    profile: &str,
    network_mode: &str,
    budget: Option<serde_json::Value>,
) -> CreateSessionParams {
    CreateSessionParams {
        profile: profile.to_string(),
        network_mode: network_mode.to_string(),
        capture_policy: None,
        seed: None,
        budget,
        no_blocklist: false,
    }
}

// --- AC-PROFVAL-01 — unknown profile ------------------------------------

#[tokio::test]
async fn ac_profval_01_unknown_profile_rejected_with_typed_envelope() {
    let h = make_handlers();
    let p = create_params("nonexistent", "live", None);
    let err = h.session_create(p).await.expect_err("must reject");
    assert_eq!(err.code, LoomErrorCode::UnknownProfile);
    let data = err.data.as_ref().expect("envelope must carry data");
    assert_eq!(data["provided"], "nonexistent");
    assert!(
        data["available"].is_array(),
        "data.available must be an array; got: {data}"
    );
}

// --- AC-PROFVAL-02 — invalid network mode -------------------------------

#[tokio::test]
async fn ac_profval_02_invalid_network_mode_rejected_with_typed_envelope() {
    let h = make_handlers();
    let p = create_params("safe", "bogus", None);
    let err = h.session_create(p).await.expect_err("must reject");
    assert_eq!(err.code, LoomErrorCode::InvalidNetworkMode);
    let data = err.data.as_ref().expect("envelope must carry data");
    assert_eq!(data["provided"], "bogus");
    assert!(data["available"].is_array());
}

// --- AC-PROFVAL-03 — invalid budget key (server side) -------------------

#[tokio::test]
async fn ac_profval_03_invalid_budget_key_rejected_with_typed_envelope() {
    let h = make_handlers();
    let p = create_params("safe", "live", Some(serde_json::json!({"garbage": 5})));
    let err = h.session_create(p).await.expect_err("must reject");
    assert_eq!(err.code, LoomErrorCode::InvalidBudgetKey);
    let data = err.data.as_ref().expect("envelope must carry data");
    assert_eq!(data["provided"], "garbage");
    assert!(data["available"].is_array());
}

// --- AC-PROFVAL-04 — every rejection emits a typed envelope -------------

/// Sweep: each rejection class produces an envelope where `code` is its
/// canonical typed variant (snake_case on the wire) and `data.provided`
/// matches the offending value. Single integration test asserting the
/// AC-PROFVAL-04 contract umbrella over -01/-02/-03.
#[tokio::test]
async fn ac_profval_04_each_rejection_class_serialises_typed_envelope() {
    let h = make_handlers();

    // Profile
    let err = h
        .session_create(create_params("nonexistent", "live", None))
        .await
        .expect_err("profile must reject");
    let body = serde_json::to_value(&err).unwrap();
    assert_eq!(body["code"], "unknown_profile");
    assert_eq!(body["data"]["provided"], "nonexistent");

    // Network mode
    let err = h
        .session_create(create_params("safe", "bogus", None))
        .await
        .expect_err("network_mode must reject");
    let body = serde_json::to_value(&err).unwrap();
    assert_eq!(body["code"], "invalid_network_mode");
    assert_eq!(body["data"]["provided"], "bogus");

    // Budget key
    let err = h
        .session_create(create_params(
            "safe",
            "live",
            Some(serde_json::json!({"garbage": 5})),
        ))
        .await
        .expect_err("budget key must reject");
    let body = serde_json::to_value(&err).unwrap();
    assert_eq!(body["code"], "invalid_budget_key");
    assert_eq!(body["data"]["provided"], "garbage");
}

// --- Negative control: canonical inputs reach the adapter ---------------

/// With canonical profile/network_mode and no budget, validation must
/// pass and the call must reach the (NoopCoreBridge) adapter — yielding
/// `InternalError` rather than any of the typed PROFVAL codes. Proves
/// the validator does not over-reject canonical inputs.
#[tokio::test]
async fn canonical_inputs_pass_validation_and_reach_adapter() {
    let h = make_handlers();
    let p = create_params("safe", "live", None);
    let err = h.session_create(p).await.expect_err("noop adapter errors");
    // Adapter-side error — proves we got past validation.
    assert_eq!(err.code, LoomErrorCode::InternalError);
}
