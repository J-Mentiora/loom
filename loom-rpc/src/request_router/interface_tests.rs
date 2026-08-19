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

// web.scroll: `selector` is OPTIONAL — "scroll the page" needs no selector.
#[test]
fn web_scroll_parses_without_selector() {
    let action = super::parse_action(
        "web.scroll",
        serde_json::json!({ "session_id": "01J0000000000000000000000A", "delta_y": 400 }),
    )
    .expect("web.scroll must parse without a selector");
    match action {
        super::Action::WebScroll {
            selector, delta_y, ..
        } => {
            assert_eq!(selector, None, "absent selector must parse as None");
            assert_eq!(delta_y, Some(400));
        }
        other => panic!("expected WebScroll, got {other:?}"),
    }
}

// Back-compat: a selector still parses (now as Some).
#[test]
fn web_scroll_still_parses_with_selector() {
    let action = super::parse_action(
        "web.scroll",
        serde_json::json!({ "session_id": "01J0000000000000000000000A", "selector": "body", "delta_y": 400 }),
    )
    .expect("web.scroll must still parse with a selector");
    match action {
        super::Action::WebScroll { selector, .. } => {
            assert_eq!(selector.as_deref(), Some("body"));
        }
        other => panic!("expected WebScroll, got {other:?}"),
    }
}

// interactive-settle-bounded: `until` is honored on web.click.
#[test]
fn web_click_parses_until() {
    let action = super::parse_action(
        "web.click",
        serde_json::json!({ "session_id": "01J0000000000000000000000A", "selector": "#go", "until": "load" }),
    )
    .expect("web.click with until must parse");
    match action {
        super::Action::WebClick { until, .. } => {
            assert_eq!(
                until.as_deref(),
                Some("load"),
                "until must reach the action"
            );
        }
        other => panic!("expected WebClick, got {other:?}"),
    }
}

// Back-compat: web.click without `until` parses as None (daemon default = settled).
#[test]
fn web_click_defaults_until_to_none() {
    let action = super::parse_action(
        "web.click",
        serde_json::json!({ "session_id": "01J0000000000000000000000A", "selector": "#go" }),
    )
    .expect("web.click without until must still parse");
    match action {
        super::Action::WebClick { until, .. } => assert_eq!(until, None),
        other => panic!("expected WebClick, got {other:?}"),
    }
}

// interactive-settle-bounded: `until` is honored on web.type.
#[test]
fn web_type_parses_until() {
    let action = super::parse_action(
        "web.type",
        serde_json::json!({ "session_id": "01J0000000000000000000000A", "selector": "#email", "text": "a@example.com", "until": "networkidle" }),
    )
    .expect("web.type with until must parse");
    match action {
        super::Action::WebType { until, .. } => {
            assert_eq!(until.as_deref(), Some("networkidle"));
        }
        other => panic!("expected WebType, got {other:?}"),
    }
}

// interactive-settle-bounded: an invalid `until` is rejected loud (no coercion),
// same posture as web.navigate — so a churny SPA never gets a silently-wrong gate.
#[test]
fn web_click_rejects_invalid_until() {
    let err = super::parse_action(
        "web.click",
        serde_json::json!({ "session_id": "01J0000000000000000000000A", "selector": "#go", "until": "whenever" }),
    )
    .expect_err("invalid until must be rejected");
    assert!(
        err.message.contains("until"),
        "error should name the offending param, got: {}",
        err.message
    );
}
