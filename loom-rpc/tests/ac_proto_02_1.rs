//! AC-PROTO-02.1 — Schema validation rejects malformed requests.
//!
//! Given the JSON-RPC server,
//! When a client sends `{method: "web.click", params: {selector: 42}}`
//! (selector should be string),
//! Then the response is `{error: {code: "schema_violation",
//! data: {field: "params.selector", expected: "string", actual: "integer"}}}`.

use loom_rpc::{
    schema_provider::schema_provider::{CompiledJsonSchema, SchemaProviderApi, SchemaRegistry},
    schema_validator::schema_validator::{SchemaValidator, SchemaValidatorApi, ValidationOutcome},
};
use serde_json::json;
use std::sync::Arc;

/// Minimal in-test SchemaProvider that serves a single method's schema.
struct OneMethodProvider {
    method: String,
    request_schema: serde_json::Value,
}

impl SchemaProviderApi for OneMethodProvider {
    fn lookup_request_schema(&self, method: &str) -> Option<Arc<CompiledJsonSchema>> {
        if method == self.method {
            Some(Arc::new(CompiledJsonSchema {
                inner: self.request_schema.clone(),
            }))
        } else {
            None
        }
    }
    fn lookup_response_schema(&self, _method: &str) -> Option<Arc<CompiledJsonSchema>> {
        None
    }
    fn registered_methods(&self) -> Vec<String> {
        vec![self.method.clone()]
    }
    fn get_registry_snapshot(&self) -> SchemaRegistry {
        SchemaRegistry {
            methods: vec![],
            source_wit_sha256: String::new(),
        }
    }
}

/// JSON Schema for web.click: `{selector: string}` with additionalProperties false.
fn web_click_schema() -> serde_json::Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {
            "selector": { "type": "string" }
        },
        "required": ["selector"],
        "additionalProperties": false
    })
}

#[test]
fn schema_violation_detail_on_wrong_type_param() {
    let provider = Arc::new(OneMethodProvider {
        method: "web.click".to_string(),
        request_schema: web_click_schema(),
    });
    let validator = SchemaValidator::new(provider);

    // AC-PROTO-02.1: selector is 42 (integer) but schema expects string.
    let params = json!({ "selector": 42 });
    let outcome = validator.validate_request("web.click", &params);

    match outcome {
        ValidationOutcome::Violation(err) => {
            use loom_rpc::error_translator::error_translator::LoomErrorCode;
            assert_eq!(
                err.code,
                LoomErrorCode::SchemaViolation,
                "AC-PROTO-02.1: error code must be schema_violation"
            );
            let data = err.data.expect("AC-PROTO-02.1: violation must carry data");
            assert_eq!(
                data["field"].as_str().unwrap_or(""),
                "params.selector",
                "AC-PROTO-02.1: field must be params.selector"
            );
            assert_eq!(
                data["expected"].as_str().unwrap_or(""),
                "string",
                "AC-PROTO-02.1: expected must be 'string'"
            );
            assert_eq!(
                data["actual"].as_str().unwrap_or(""),
                "integer",
                "AC-PROTO-02.1: actual must be 'integer'"
            );
        }
        ValidationOutcome::Pass => panic!("AC-PROTO-02.1: validator must reject integer selector"),
        ValidationOutcome::MethodNotFound(_) => {
            panic!("AC-PROTO-02.1: method web.click must be registered")
        }
    }
}

#[test]
fn valid_params_pass_schema_validation() {
    let provider = Arc::new(OneMethodProvider {
        method: "web.click".to_string(),
        request_schema: web_click_schema(),
    });
    let validator = SchemaValidator::new(provider);
    let params = json!({ "selector": "#button" });
    let outcome = validator.validate_request("web.click", &params);
    assert!(
        matches!(outcome, ValidationOutcome::Pass),
        "valid params must pass validation"
    );
}

#[test]
fn unknown_method_returns_method_not_found() {
    // Use a method that is neither in the provider's schema set NOR
    // in `BUILTIN_CORE_METHODS` (the validator's bypass list for
    // built-in non-action RPC methods like `session.create`,
    // `vault.grant`, `gc.run`, etc.). `web.bogus` is not in either
    // list so the validator returns MethodNotFound.
    let provider = Arc::new(OneMethodProvider {
        method: "web.click".to_string(),
        request_schema: web_click_schema(),
    });
    let validator = SchemaValidator::new(provider);
    let outcome = validator.validate_request("web.bogus", &json!({}));
    assert!(
        matches!(outcome, ValidationOutcome::MethodNotFound(_)),
        "unknown method must return MethodNotFound"
    );
}

/// Built-in core methods (`session.create`, `vault.grant`, `gc.run`,
/// etc.) bypass schema validation regardless of provider state — they
/// have no JSON Schema in the registry (the postinstall path only
/// installs per-action web.*/shell.*/etc. schemas) and are
/// authoritatively dispatched by `RequestRouter`'s match arm.
#[test]
fn builtin_core_methods_pass_validation_even_when_provider_has_only_one_method() {
    let provider = Arc::new(OneMethodProvider {
        method: "web.click".to_string(),
        request_schema: web_click_schema(),
    });
    let validator = SchemaValidator::new(provider);
    for builtin in [
        "session.create",
        "session.list",
        "session.close",
        "session.abort",
        "session.replay",
        "vault.grant",
        "vault.revoke",
        "vault.add",
        "gc.run",
        "rpc.schemas",
        "content.get",
        "health.ping",
    ] {
        let outcome = validator.validate_request(builtin, &json!({}));
        assert!(
            matches!(outcome, ValidationOutcome::Pass),
            "built-in core method {builtin} must pass validation; got {outcome:?}"
        );
    }
}
