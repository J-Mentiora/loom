//! Class-level regression guard for the v0.11.0 `web.navigate` arg-validation
//! bug: the daemon rejected documented optional params (`until`, `timeout_ms`)
//! because its on-disk schema was stale, and the schema_violation error blamed
//! `field 'params'` / `field_unknown` instead of naming the offending field and
//! method.
//!
//! Layer 1 (this file): every builtin action method's FULL documented arg set
//! must pass `SchemaValidator` against the builtin schemas — the validation-
//! side mirror of `schema_registry_drift.rs` (which checks schema *structure*,
//! not validator behavior). Layer 2: schema_violation details must name the
//! actual offending/missing field plus the method. Layer 3: `loom postinstall`
//! must refresh a stale builtin schema file instead of preserving it forever.

use loom_cli::postinstall_runner::{schema_step, BUILTIN_SCHEMAS};
use loom_rpc::{
    schema_provider::schema_provider::{CompiledJsonSchema, SchemaProviderApi, SchemaRegistry},
    schema_validator::schema_validator::{SchemaValidator, SchemaValidatorApi, ValidationOutcome},
};
use serde_json::{json, Value};
use std::sync::Arc;

/// In-test SchemaProvider serving every BUILTIN_SCHEMAS request schema —
/// the same documents a freshly postinstalled daemon compiles at startup.
struct BuiltinProvider;

fn builtin_request_schema(method: &str) -> Option<Value> {
    let (_, json_str) = BUILTIN_SCHEMAS.iter().find(|(m, _)| *m == method)?;
    let doc: Value = serde_json::from_str(json_str).expect("builtin schema is valid JSON");
    Some(doc["request"].clone())
}

impl SchemaProviderApi for BuiltinProvider {
    fn lookup_request_schema(&self, method: &str) -> Option<Arc<CompiledJsonSchema>> {
        let req = builtin_request_schema(method)?;
        Some(Arc::new(
            CompiledJsonSchema::compile(req).expect("builtin request schema must compile"),
        ))
    }
    fn lookup_response_schema(&self, _method: &str) -> Option<Arc<CompiledJsonSchema>> {
        None
    }
    fn registered_methods(&self) -> Vec<String> {
        BUILTIN_SCHEMAS.iter().map(|(m, _)| m.to_string()).collect()
    }
    fn get_registry_snapshot(&self) -> SchemaRegistry {
        SchemaRegistry {
            methods: vec![],
            source_wit_sha256: String::new(),
        }
    }
}

/// Build a type-correct sample value for one property's schema.
fn sample_value(prop_schema: &Value) -> Value {
    if let Some(first) = prop_schema["enum"].as_array().and_then(|a| a.first()) {
        return first.clone();
    }
    match prop_schema["type"].as_str() {
        Some("string") => json!("x"),
        Some("integer") => json!(1),
        Some("number") => json!(1.0),
        Some("boolean") => json!(true),
        Some("array") => {
            let item = prop_schema["items"]
                .as_object()
                .map(|_| sample_value(&prop_schema["items"]))
                .unwrap_or(json!("x"));
            json!([item])
        }
        Some("object") => json!({}),
        // Union types like ["object","null"] or untyped: an object is the
        // widest commonly-accepted shape for our schemas.
        _ => json!({}),
    }
}

/// Params object exercising EVERY declared property of a request schema.
fn full_arg_set(request_schema: &Value) -> Value {
    let mut params = serde_json::Map::new();
    if let Some(props) = request_schema["properties"].as_object() {
        for (name, prop_schema) in props {
            params.insert(name.clone(), sample_value(prop_schema));
        }
    }
    Value::Object(params)
}

/// CLASS TEST (mirrors the BUILTIN_CORE_METHODS class test, #142/#172): every
/// advertised builtin action method must accept its full documented arg set —
/// not just the required args. Pins web.navigate{url,until,timeout_ms} and all
/// optional-param siblings (scroll deltas, wait timeout_ms, get_cookies urls,
/// delete_cookies url/domain/path, ...).
#[test]
fn every_builtin_action_method_accepts_its_full_documented_arg_set() {
    let validator = SchemaValidator::new(Arc::new(BuiltinProvider));
    for (method, _) in BUILTIN_SCHEMAS {
        // rpc.schemas is a builtin core method (validator bypass) with a
        // permissive CLI-side schema; the action class is web.* here.
        if *method == "rpc.schemas" {
            continue;
        }
        let request = builtin_request_schema(method).expect("schema exists");
        let params = full_arg_set(&request);
        let outcome = validator.validate_request(method, &params);
        assert!(
            matches!(outcome, ValidationOutcome::Pass),
            "{method}: FULL documented arg set must validate clean, got {outcome:?} \
             for params {params}"
        );
    }
}

/// The exact v0.11.0 repro trio: url-only, url+until, url+until+timeout_ms.
#[test]
fn navigate_settle_capture_args_validate_clean() {
    let validator = SchemaValidator::new(Arc::new(BuiltinProvider));
    for params in [
        json!({"session": "s", "url": "https://example.test/"}),
        json!({"session": "s", "url": "https://example.test/", "until": "settled"}),
        json!({"session": "s", "url": "https://example.test/", "until": "settled", "timeout_ms": 5000}),
    ] {
        let outcome = validator.validate_request("web.navigate", &params);
        assert!(
            matches!(outcome, ValidationOutcome::Pass),
            "web.navigate must accept {params}, got {outcome:?}"
        );
    }
}

/// A deliberately bogus arg must be rejected with a detail naming the ACTUAL
/// offending field (`bogus`) and the method — not the envelope path `params`
/// with a bare `field_unknown`.
#[test]
fn unknown_arg_violation_names_the_offending_field_and_method() {
    let validator = SchemaValidator::new(Arc::new(BuiltinProvider));
    let params = json!({"session": "s", "url": "https://example.test/", "bogus": 1});
    match validator.validate_request("web.navigate", &params) {
        ValidationOutcome::Violation(err) => {
            let data = err.data.clone().expect("violation must carry data");
            assert_eq!(
                data["field"].as_str().unwrap_or(""),
                "bogus",
                "violation must name the offending field, got detail {data} \
                 (message: {})",
                err.message
            );
            assert!(
                err.message.contains("web.navigate"),
                "message must name the method, got: {}",
                err.message
            );
            assert!(
                err.message.contains("bogus"),
                "message must name the offending field, got: {}",
                err.message
            );
        }
        other => panic!("bogus arg must be a Violation, got {other:?}"),
    }
}

/// A missing required field must be named in the detail (`url`), not blamed on
/// the envelope path `params`.
#[test]
fn missing_required_violation_names_the_missing_field() {
    let validator = SchemaValidator::new(Arc::new(BuiltinProvider));
    let params = json!({"session": "s"});
    match validator.validate_request("web.navigate", &params) {
        ValidationOutcome::Violation(err) => {
            let data = err.data.clone().expect("violation must carry data");
            assert_eq!(
                data["field"].as_str().unwrap_or(""),
                "url",
                "violation must name the missing field, got detail {data} \
                 (message: {})",
                err.message
            );
        }
        other => panic!("missing url must be a Violation, got {other:?}"),
    }
}

/// `loom postinstall` must REFRESH a stale builtin schema file whose content
/// no longer matches the binary's builtin — per-method idempotence ("file
/// exists → skip") is exactly what froze a pre-settle-capture
/// web.navigate.json (no `until`/`timeout_ms`) across upgrades while newer
/// method files were written fresh, producing the navigate-vs-wait_for
/// asymmetry.
#[test]
fn schema_step_refreshes_stale_builtin_content() {
    let dir = tempfile::tempdir().expect("tempdir");
    // The pre-settle-capture navigate schema as found on the repro machine.
    let stale = r#"{"request":{"type":"object","properties":{"session":{"type":"string"},"url":{"type":"string"}},"required":["session","url"],"additionalProperties":false},"response":{"type":"object"}}"#;
    std::fs::write(dir.path().join("web.navigate.json"), stale).expect("seed stale file");

    schema_step(dir.path()).expect("schema_step succeeds");

    let on_disk =
        std::fs::read_to_string(dir.path().join("web.navigate.json")).expect("file exists");
    let (_, builtin) = BUILTIN_SCHEMAS
        .iter()
        .find(|(m, _)| *m == "web.navigate")
        .expect("builtin navigate entry");
    assert_eq!(
        on_disk, *builtin,
        "stale web.navigate.json must be refreshed to the current builtin content \
         (per-method idempotence must not preserve outdated schemas)"
    );
}
