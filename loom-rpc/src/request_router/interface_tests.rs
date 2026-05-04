// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-rpc/modules/request_router/interface_tests.rs` instead.
// Interface tests for `RequestRouter`. Verifies SR-RPC-03 startup
// enumeration, IC-RPC-03 pre-dispatch validation wiring, FR-PROTO-01
// refuse-to-start on missing handler/schema.

use super::request_router::{
    RegistrationError, RequestRouter, RequestRouterApi, RouterContext,
};
use crate::rpc_handlers::rpc_handlers::RpcHandlers;
use crate::schema_provider::schema_provider::SchemaProviderApi;
use crate::schema_validator::schema_validator::SchemaValidatorApi;
use jsonrpsee::RpcModule;
use std::sync::Arc;

#[test]
fn router_context_holds_handlers_and_validator() {
    fn _ck(c: &RouterContext) {
        let _: &Arc<RpcHandlers> = &c.handlers;
        let _: &Arc<dyn SchemaValidatorApi> = &c.validator;
    }
    let _ = _ck;
}

#[test]
fn register_methods_signature_takes_handlers_schemas_validator() {
    fn _ck(
        h: Arc<RpcHandlers>,
        s: Arc<dyn SchemaProviderApi>,
        v: Arc<dyn SchemaValidatorApi>,
    ) -> Result<Arc<RequestRouter>, RegistrationError> {
        RequestRouter::register_methods(h, s, v)
    }
    let _ = _ck;
}

#[test]
fn registration_error_distinguishes_handler_missing_schema_missing_jsonrpsee() {
    // FR-PROTO-01: refuse-to-start at startup, not at request time.
    let _ = RegistrationError::HandlerMissing {
        method: "session.create".into(),
    };
    let _ = RegistrationError::SchemaMissing {
        method: "session.create".into(),
    };
    let _ = RegistrationError::JsonRpsee {
        reason: "duplicate".into(),
    };
}

#[test]
fn methods_accessor_returns_registered_method_names() {
    // Used by `loom serve` startup audit line.
    fn _ck<R: RequestRouterApi>(r: &R) -> Vec<String> {
        r.methods()
    }
    let _ = _ck::<RequestRouter>;
}

#[test]
fn dispatch_signature_takes_method_and_params_returns_bytes() {
    fn _ck() {
        async fn _go<R: RequestRouterApi>(
            r: &R,
            m: &str,
            p: serde_json::Value,
        ) -> Vec<u8> {
            r.dispatch(m, p).await
        }
        let _ = _go::<RequestRouter>;
    }
    let _ = _ck;
}

#[test]
fn module_accessor_returns_arc_rpc_module_for_mcp_reuse() {
    // UX-13: McpAdapter reuses the RpcModule so the JSON-RPC and MCP
    // tool lists are bit-equal.
    fn _ck(r: &RequestRouter) -> Arc<RpcModule<RouterContext>> {
        r.module()
    }
    let _ = _ck;
}
