// Interface tests for `RequestRouter`. Verifies startup
// enumeration, pre-dispatch validation wiring,
// refuse-to-start on missing handler/schema.

use super::request_router::{RegistrationError, RequestRouter, RequestRouterApi, RouterContext};
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
    // refuse-to-start at startup, not at request time.
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
        async fn _go<R: RequestRouterApi>(r: &R, m: &str, p: serde_json::Value) -> Vec<u8> {
            r.dispatch(m, p).await
        }
        let _ = _go::<RequestRouter>;
    }
    let _ = _ck;
}

#[test]
fn module_accessor_returns_arc_rpc_module_for_mcp_reuse() {
    // McpAdapter reuses the RpcModule so the JSON-RPC and MCP
    // tool lists are bit-equal.
    fn _ck(r: &RequestRouter) -> Arc<RpcModule<RouterContext>> {
        r.module()
    }
    let _ = _ck;
}

// === session.replay `speed` param (regression: only `as_f64` was
// accepted, so the string forms older CLIs forwarded were silently
// dropped — speed was always None) ===

#[test]
fn replay_speed_accepts_numbers() {
    let p = serde_json::json!({ "speed": 2.5 });
    assert_eq!(super::optional_replay_speed(&p).unwrap(), Some(2.5_f32));
    let p = serde_json::json!({ "speed": 1 });
    assert_eq!(super::optional_replay_speed(&p).unwrap(), Some(1.0_f32));
    // 0 is the documented "max"/unpaced sentinel — valid.
    let p = serde_json::json!({ "speed": 0 });
    assert_eq!(super::optional_replay_speed(&p).unwrap(), Some(0.0_f32));
}

#[test]
fn replay_speed_absent_or_null_is_none() {
    assert_eq!(
        super::optional_replay_speed(&serde_json::json!({})).unwrap(),
        None
    );
    let p = serde_json::json!({ "speed": null });
    assert_eq!(super::optional_replay_speed(&p).unwrap(), None);
}

#[test]
fn replay_speed_accepts_legacy_cli_string_forms() {
    // CLIs ≤ 0.10.1 forwarded the documented strings verbatim.
    let p = serde_json::json!({ "speed": "realtime" });
    assert_eq!(super::optional_replay_speed(&p).unwrap(), Some(1.0_f32));
    let p = serde_json::json!({ "speed": "2x" });
    assert_eq!(super::optional_replay_speed(&p).unwrap(), Some(2.0_f32));
    let p = serde_json::json!({ "speed": "1.5x" });
    assert_eq!(super::optional_replay_speed(&p).unwrap(), Some(1.5_f32));
    let p = serde_json::json!({ "speed": "max" });
    assert_eq!(super::optional_replay_speed(&p).unwrap(), Some(0.0_f32));
}

#[test]
fn replay_speed_rejects_garbage_loudly() {
    use crate::error_translator::error_translator::LoomErrorCode;
    for bad in [
        serde_json::json!({ "speed": "fast" }),
        serde_json::json!({ "speed": "-2x" }),
        serde_json::json!({ "speed": true }),
        serde_json::json!({ "speed": ["2x"] }),
    ] {
        let err = super::optional_replay_speed(&bad)
            .expect_err(&format!("speed {bad} must be rejected, not coerced"));
        assert_eq!(err.code, LoomErrorCode::SchemaViolation);
        assert!(
            err.message.contains("speed"),
            "error must name the param: {}",
            err.message
        );
    }
}
