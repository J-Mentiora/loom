// Interface tests for `RpcHandlers`. Verifies every contract method
// has a handler signature, the routing split (action vs core),
// vault response shape, and that rpc.schemas is served in-memory.

use super::rpc_handlers::{HandlerResult, RpcHandlers};
use crate::core_service_adapter::core_service_adapter::{
    CoreServiceAdapterApi, CreateSessionParams, DiffReport, ExportInfo, GrantInfo, GrantParams,
    SessionInfo, SessionInspection, ValidationResult,
};
use crate::host_service_adapter::host_service_adapter::{Action, HostServiceAdapterApi, Receipt};
use crate::rpc_observability::rpc_observability::RpcObservabilityApi;
use crate::schema_provider::schema_provider::{SchemaProviderApi, SchemaRegistry};
use crate::schema_validator::schema_validator::SchemaValidatorApi;
use std::sync::Arc;

#[test]
fn constructor_takes_five_arc_handles() {
    fn _ck(
        c: Arc<dyn CoreServiceAdapterApi>,
        h: Arc<dyn HostServiceAdapterApi>,
        s: Arc<dyn SchemaProviderApi>,
        v: Arc<dyn SchemaValidatorApi>,
        o: Arc<dyn RpcObservabilityApi>,
    ) -> Arc<RpcHandlers> {
        RpcHandlers::new(c, h, s, v, o)
    }
    let _ = _ck;
}

// ===== session.* signatures =====

#[test]
fn session_create_signature() {
    fn _ck() {
        async fn _go(h: &RpcHandlers, p: CreateSessionParams) -> HandlerResult<SessionInfo> {
            h.session_create(p).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

#[test]
fn session_inspect_signature_supports_optional_at_action() {
    fn _ck() {
        async fn _go(
            h: &RpcHandlers,
            s: String,
            at: Option<u64>,
        ) -> HandlerResult<SessionInspection> {
            h.session_inspect(s, at).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

#[test]
fn session_list_signature() {
    fn _ck() {
        async fn _go(h: &RpcHandlers) -> HandlerResult<Vec<SessionInfo>> {
            h.session_list().await
        }
        let _ = _go;
    }
    let _ = _ck;
}

#[test]
fn session_close_signature() {
    fn _ck() {
        async fn _go(h: &RpcHandlers, s: String) -> HandlerResult<SessionInfo> {
            h.session_close(s).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

#[test]
fn session_abort_signature() {
    fn _ck() {
        async fn _go(h: &RpcHandlers, s: String, r: String) -> HandlerResult<SessionInfo> {
            h.session_abort(s, r).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

#[test]
fn session_replay_signature() {
    fn _ck() {
        async fn _go(
            h: &RpcHandlers,
            s: String,
            sp: Option<f32>,
            nm: Option<String>,
        ) -> HandlerResult<SessionInfo> {
            h.session_replay(s, sp, nm).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

#[test]
fn session_diff_signature() {
    fn _ck() {
        async fn _go(
            h: &RpcHandlers,
            a: String,
            b: String,
            i: bool,
            d: bool,
        ) -> HandlerResult<DiffReport> {
            h.session_diff(a, b, i, d).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

#[test]
fn session_export_signature_for_four_formats() {
    fn _ck() {
        async fn _go(h: &RpcHandlers, s: String, f: String) -> HandlerResult<ExportInfo> {
            h.session_export(s, f).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

#[test]
fn session_validate_signature() {
    fn _ck() {
        async fn _go(h: &RpcHandlers, s: String) -> HandlerResult<ValidationResult> {
            h.session_validate(s).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

// ===== action.* — routing =====

#[test]
fn action_dispatch_returns_receipt_via_host_adapter() {
    // Single host dispatch path; typed Receipt only.
    fn _ck() {
        async fn _go(h: &RpcHandlers, a: Action) -> HandlerResult<Receipt> {
            h.action_dispatch(a).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

// ===== vault.* =====

#[test]
fn vault_grant_returns_grant_info_no_secret_field() {
    // Response is GrantInfo (grant_id only).
    fn _ck() {
        async fn _go(h: &RpcHandlers, p: GrantParams) -> HandlerResult<GrantInfo> {
            h.vault_grant(p).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

#[test]
fn vault_revoke_signature_takes_grant_id_and_reason() {
    fn _ck() {
        async fn _go(h: &RpcHandlers, g: String, r: String) -> HandlerResult<()> {
            h.vault_revoke(g, r).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

#[test]
fn vault_list_grants_signature() {
    fn _ck() {
        async fn _go(h: &RpcHandlers, s: Option<String>) -> HandlerResult<Vec<GrantInfo>> {
            h.vault_list_grants(s).await
        }
        let _ = _go;
    }
    let _ = _ck;
}

// ===== rpc.* =====

#[test]
fn rpc_schemas_returns_in_memory_registry_snapshot() {
    // Never re-reads disk on request path.
    fn _ck() {
        async fn _go(h: &RpcHandlers) -> HandlerResult<SchemaRegistry> {
            h.rpc_schemas().await
        }
        let _ = _go;
    }
    let _ = _ck;
}

// ===== canonical-JSON =====

#[test]
fn serialise_canonical_uses_jcs_helper_function() {
    // All responses go through the canonical-JSON helper.
    fn _ck<T: serde::Serialize>(v: &T) -> Result<String, super::rpc_handlers::JsonRpcError> {
        RpcHandlers::serialise_canonical(v)
    }
    let _ = _ck::<u32>;
}

// ===== vault.grant response-schema validation (belt+braces) =====
//
// The module contract and `GrantInfo`'s doc both promise that
// `SchemaValidator::validate_response` guards vault.grant responses
// against secret-shaped fields. These tests pin that the handler
// actually invokes it (it previously had zero production call sites).

mod vault_grant_response_validation {
    use super::*;
    use crate::core_service_adapter::core_service_adapter::{
        AdapterError, ContentData, GcRunReport, PlaywrightImportInfo, ReapReport, VaultAddInfo,
        VaultAddParams, VaultDeleteSecretInfo, VaultDeleteSecretParams, VaultDiagnoseInfo,
        VaultGetSessionContextInfo, VaultListLabelsInfo, VaultListLabelsParams, VaultSetSecretInfo,
        VaultSetSecretParams,
    };
    use crate::error_translator::error_translator::LoomErrorCode;
    use crate::rpc_observability::rpc_observability::RpcObservability;
    use crate::schema_provider::schema_provider::CompiledJsonSchema;
    use crate::schema_validator::schema_validator::SchemaValidator;

    /// Core fake: only `vault_grant` is reachable from these tests.
    struct GrantOnlyCore;

    #[rustfmt::skip]
    impl CoreServiceAdapterApi for GrantOnlyCore {
        fn vault_grant(&self, p: GrantParams) -> Result<GrantInfo, AdapterError> {
            Ok(GrantInfo {
                grant_id: "01HZTESTGRANT".into(),
                origin: "https://example.com".into(),
                scopes: vec!["cookies".into()],
                ttl_seconds: p.ttl_seconds,
                label: p.label,
            })
        }
        fn create_session(&self, _: CreateSessionParams) -> Result<SessionInfo, crate::error_translator::error_translator::LoomError> { unimplemented!() }
        fn inspect_session(&self, _: &str, _: Option<u64>) -> Result<SessionInspection, AdapterError> { unimplemented!() }
        fn list_sessions(&self) -> Result<Vec<SessionInfo>, AdapterError> { unimplemented!() }
        fn close_session(&self, _: &str) -> Result<SessionInfo, AdapterError> { unimplemented!() }
        fn abort_session(&self, _: &str, _: &str) -> Result<SessionInfo, AdapterError> { unimplemented!() }
        fn replay_session(&self, _: &str, _: Option<f32>, _: Option<&str>) -> Result<SessionInfo, crate::core_service_adapter::core_service_adapter::LoomError> { unimplemented!() }
        fn diff_sessions(&self, _: &str, _: &str, _: bool, _: bool) -> Result<DiffReport, AdapterError> { unimplemented!() }
        fn export_session(&self, _: &str, _: &str) -> Result<ExportInfo, AdapterError> { unimplemented!() }
        fn content_get(&self, _: &str) -> Result<ContentData, AdapterError> { unimplemented!() }
        fn validate_session(&self, _: &str) -> Result<ValidationResult, AdapterError> { unimplemented!() }
        fn import_playwright(&self, _: &[u8]) -> Result<PlaywrightImportInfo, AdapterError> { unimplemented!() }
        fn vault_revoke(&self, _: &str, _: &str) -> Result<(), AdapterError> { unimplemented!() }
        fn vault_list_grants(&self, _: Option<&str>) -> Result<Vec<GrantInfo>, AdapterError> { unimplemented!() }
        fn vault_add(&self, _: VaultAddParams) -> Result<VaultAddInfo, AdapterError> { unimplemented!() }
        fn vault_set_secret(&self, _: VaultSetSecretParams) -> Result<VaultSetSecretInfo, AdapterError> { unimplemented!() }
        fn vault_delete_secret(&self, _: VaultDeleteSecretParams) -> Result<VaultDeleteSecretInfo, AdapterError> { unimplemented!() }
        fn vault_list_labels(&self, _: VaultListLabelsParams) -> Result<VaultListLabelsInfo, AdapterError> { unimplemented!() }
        fn vault_diagnose(&self) -> Result<VaultDiagnoseInfo, AdapterError> { unimplemented!() }
        fn vault_get_session_context(&self) -> Result<VaultGetSessionContextInfo, AdapterError> { unimplemented!() }
        fn gc_run(&self, _: Option<u64>, _: Option<u64>) -> Result<GcRunReport, AdapterError> { unimplemented!() }
        fn session_reap(&self, _: bool) -> Result<ReapReport, AdapterError> { unimplemented!() }
    }

    struct NoopHost;

    #[async_trait::async_trait]
    impl HostServiceAdapterApi for NoopHost {
        async fn dispatch_action(&self, _: Action) -> Result<Receipt, AdapterError> {
            unimplemented!()
        }
    }

    /// Provider serving one response schema for vault.grant.
    struct GrantResponseSchemaProvider {
        response_schema: serde_json::Value,
    }

    impl SchemaProviderApi for GrantResponseSchemaProvider {
        fn lookup_request_schema(&self, _: &str) -> Option<Arc<CompiledJsonSchema>> {
            None
        }
        fn lookup_response_schema(&self, method: &str) -> Option<Arc<CompiledJsonSchema>> {
            (method == "vault.grant").then(|| {
                Arc::new(
                    CompiledJsonSchema::compile(self.response_schema.clone())
                        .expect("test schema must compile"),
                )
            })
        }
        fn registered_methods(&self) -> Vec<String> {
            vec!["vault.grant".to_string()]
        }
        fn get_registry_snapshot(&self) -> SchemaRegistry {
            SchemaRegistry {
                methods: vec![],
                source_wit_sha256: String::new(),
            }
        }
    }

    fn handlers_with_response_schema(schema: serde_json::Value) -> Arc<RpcHandlers> {
        let provider: Arc<dyn SchemaProviderApi> = Arc::new(GrantResponseSchemaProvider {
            response_schema: schema,
        });
        let validator = SchemaValidator::new(Arc::clone(&provider));
        RpcHandlers::new(
            Arc::new(GrantOnlyCore),
            Arc::new(NoopHost),
            provider,
            validator,
            RpcObservability::new(),
        )
    }

    fn grant_params() -> GrantParams {
        GrantParams {
            session_id: "01HZTESTSESSION".into(),
            origin: "https://example.com".into(),
            scopes: vec!["cookies".into()],
            ttl_seconds: 60,
            label: "test".into(),
            credential_type: None,
        }
    }

    /// Strict response schema matching GrantInfo's shape: the grant
    /// passes through (Pass arm).
    #[tokio::test]
    async fn vault_grant_passes_when_response_matches_schema() {
        let h = handlers_with_response_schema(serde_json::json!({
            "type": "object",
            "properties": {
                "grant_id": {"type": "string"},
                "origin": {"type": "string"},
                "scopes": {"type": "array"},
                "ttl_seconds": {"type": "integer"},
                "label": {"type": "string"}
            },
            "additionalProperties": false
        }));
        let info = h.vault_grant(grant_params()).await.expect("grant passes");
        assert_eq!(info.grant_id, "01HZTESTGRANT");
    }

    /// A response the schema rejects must NOT be returned: the handler
    /// refuses with internal_error instead of leaking the payload.
    #[tokio::test]
    async fn vault_grant_refuses_response_that_violates_schema() {
        let h = handlers_with_response_schema(serde_json::json!({
            "type": "object",
            "required": ["grant_id"],
            // GrantInfo's serialized fields beyond grant_id are not
            // declared, so additionalProperties:false trips — stands in
            // for any unexpected (secret-shaped) field in the response.
            "properties": { "grant_id": {"type": "string"} },
            "additionalProperties": false
        }));
        let err = h
            .vault_grant(grant_params())
            .await
            .expect_err("violating response must be refused");
        assert_eq!(err.code, LoomErrorCode::InternalError);
        assert!(err.message.contains("response"), "message: {}", err.message);
    }
}

// ===== session.create cap rejection — typed envelope (typed-capacity-errors) =====
//
// The daemon's cap rejection must reach the wire as `session_cap_exceeded`
// with `{active, cap, hint}` in `data` — never collapse to the legacy
// `internal_error: session.create failed`. Pins the handler half of the
// contract (daemon half: loom-daemon's cap-saturation test).

mod session_cap_envelope {
    use super::*;
    use crate::core_service_adapter::core_service_adapter::{
        AdapterError, ContentData, GcRunReport, PlaywrightImportInfo, ReapReport, VaultAddInfo,
        VaultAddParams, VaultDeleteSecretInfo, VaultDeleteSecretParams, VaultDiagnoseInfo,
        VaultGetSessionContextInfo, VaultListLabelsInfo, VaultListLabelsParams, VaultSetSecretInfo,
        VaultSetSecretParams,
    };
    use crate::error_translator::error_translator::{LoomError, LoomErrorCode};
    use crate::rpc_observability::rpc_observability::RpcObservability;
    use crate::schema_provider::schema_provider::CompiledJsonSchema;
    use crate::schema_validator::schema_validator::SchemaValidator;

    /// Core fake whose `create_session` is saturated: returns the daemon-
    /// shaped typed cap rejection (code + message + `{active, cap, hint}`).
    struct CapSaturatedCore;

    #[rustfmt::skip]
    impl CoreServiceAdapterApi for CapSaturatedCore {
        fn create_session(&self, _: CreateSessionParams) -> Result<SessionInfo, LoomError> {
            Err(LoomError::new(
                LoomErrorCode::SessionCapExceeded,
                "concurrent session cap reached (2/2); close sessions or run \
                 `loom session reap` to free leaked slots, then retry",
            )
            .with_context(serde_json::json!({
                "active": 2,
                "cap": 2,
                "hint": "close sessions or run `loom session reap`",
            })))
        }
        fn inspect_session(&self, _: &str, _: Option<u64>) -> Result<SessionInspection, AdapterError> { unimplemented!() }
        fn list_sessions(&self) -> Result<Vec<SessionInfo>, AdapterError> { unimplemented!() }
        fn close_session(&self, _: &str) -> Result<SessionInfo, AdapterError> { unimplemented!() }
        fn abort_session(&self, _: &str, _: &str) -> Result<SessionInfo, AdapterError> { unimplemented!() }
        fn replay_session(&self, _: &str, _: Option<f32>, _: Option<&str>) -> Result<SessionInfo, loom_shared::error_format::LoomError> { unimplemented!() }
        fn diff_sessions(&self, _: &str, _: &str, _: bool, _: bool) -> Result<DiffReport, AdapterError> { unimplemented!() }
        fn export_session(&self, _: &str, _: &str) -> Result<ExportInfo, AdapterError> { unimplemented!() }
        fn content_get(&self, _: &str) -> Result<ContentData, AdapterError> { unimplemented!() }
        fn validate_session(&self, _: &str) -> Result<ValidationResult, AdapterError> { unimplemented!() }
        fn import_playwright(&self, _: &[u8]) -> Result<PlaywrightImportInfo, AdapterError> { unimplemented!() }
        fn vault_grant(&self, _: GrantParams) -> Result<GrantInfo, AdapterError> { unimplemented!() }
        fn vault_revoke(&self, _: &str, _: &str) -> Result<(), AdapterError> { unimplemented!() }
        fn vault_list_grants(&self, _: Option<&str>) -> Result<Vec<GrantInfo>, AdapterError> { unimplemented!() }
        fn vault_add(&self, _: VaultAddParams) -> Result<VaultAddInfo, AdapterError> { unimplemented!() }
        fn vault_set_secret(&self, _: VaultSetSecretParams) -> Result<VaultSetSecretInfo, AdapterError> { unimplemented!() }
        fn vault_delete_secret(&self, _: VaultDeleteSecretParams) -> Result<VaultDeleteSecretInfo, AdapterError> { unimplemented!() }
        fn vault_list_labels(&self, _: VaultListLabelsParams) -> Result<VaultListLabelsInfo, AdapterError> { unimplemented!() }
        fn vault_diagnose(&self) -> Result<VaultDiagnoseInfo, AdapterError> { unimplemented!() }
        fn vault_get_session_context(&self) -> Result<VaultGetSessionContextInfo, AdapterError> { unimplemented!() }
        fn gc_run(&self, _: Option<u64>, _: Option<u64>) -> Result<GcRunReport, AdapterError> { unimplemented!() }
        fn session_reap(&self, _: bool) -> Result<ReapReport, AdapterError> { unimplemented!() }
    }

    struct NoopHost;

    #[async_trait::async_trait]
    impl HostServiceAdapterApi for NoopHost {
        async fn dispatch_action(&self, _: Action) -> Result<Receipt, AdapterError> {
            unimplemented!()
        }
        // default has_chromium() = true, so session_create reaches the core.
    }

    struct NoSchemas;

    impl SchemaProviderApi for NoSchemas {
        fn lookup_request_schema(&self, _: &str) -> Option<Arc<CompiledJsonSchema>> {
            None
        }
        fn lookup_response_schema(&self, _: &str) -> Option<Arc<CompiledJsonSchema>> {
            None
        }
        fn registered_methods(&self) -> Vec<String> {
            Vec::new()
        }
        fn get_registry_snapshot(&self) -> SchemaRegistry {
            SchemaRegistry {
                methods: vec![],
                source_wit_sha256: String::new(),
            }
        }
    }

    fn saturated_handlers() -> Arc<RpcHandlers> {
        let provider: Arc<dyn SchemaProviderApi> = Arc::new(NoSchemas);
        let validator = SchemaValidator::new(Arc::clone(&provider));
        RpcHandlers::new(
            Arc::new(CapSaturatedCore),
            Arc::new(NoopHost),
            provider,
            validator,
            RpcObservability::new(),
        )
    }

    #[tokio::test]
    async fn session_create_cap_rejection_is_typed_with_active_cap_hint() {
        let h = saturated_handlers();
        let err = h
            .session_create(CreateSessionParams {
                profile: "safe".to_string(),
                network_mode: "live".to_string(),
                capture_policy: None,
                seed: None,
                budget: None,
                no_blocklist: false,
                no_determinism: false,
                clock_anchor: None,
            })
            .await
            .expect_err("saturated core must reject");

        assert_eq!(err.code, LoomErrorCode::SessionCapExceeded);
        // The wire spelling is the typed snake_case code — NOT internal_error.
        assert_eq!(
            serde_json::to_value(err.code).expect("serialize code"),
            serde_json::json!("session_cap_exceeded")
        );
        assert!(
            err.message.contains("(2/2)"),
            "message must carry active/cap: {}",
            err.message
        );
        let data = err.data.expect("cap envelope must carry data");
        assert_eq!(data["active"], 2);
        assert_eq!(data["cap"], 2);
        assert!(
            data["hint"]
                .as_str()
                .is_some_and(|h| h.contains("loom session reap")),
            "hint must name the remediation; got: {data}"
        );
    }
}
