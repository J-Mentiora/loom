//! Schema validation rejects malformed requests.
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
            Some(Arc::new(
                CompiledJsonSchema::compile(self.request_schema.clone())
                    .expect("test schema must compile"),
            ))
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

    // selector is 42 (integer) but schema expects string.
    let params = json!({ "selector": 42 });
    let outcome = validator.validate_request("web.click", &params);

    match outcome {
        ValidationOutcome::Violation(err) => {
            use loom_rpc::error_translator::error_translator::LoomErrorCode;
            assert_eq!(
                err.code,
                LoomErrorCode::SchemaViolation,
                "error code must be schema_violation"
            );
            let data = err.data.expect("violation must carry data");
            assert_eq!(
                data["field"].as_str().unwrap_or(""),
                "params.selector",
                "field must be params.selector"
            );
            assert_eq!(
                data["expected"].as_str().unwrap_or(""),
                "string",
                "expected must be 'string'"
            );
            assert_eq!(
                data["actual"].as_str().unwrap_or(""),
                "integer",
                "actual must be 'integer'"
            );
        }
        ValidationOutcome::Pass => panic!("validator must reject integer selector"),
        ValidationOutcome::MethodNotFound(_) => {
            panic!("method web.click must be registered")
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
    // The full BUILTIN_CORE_METHODS set — every method with a
    // hand-written `RequestRouter::dispatch` arm. Keep this list in
    // lockstep with the const: the regression where `loom vault
    // add-direct/delete/list-labels/diagnose` and `loom session reap`
    // failed with `method_not_found` on any postinstalled daemon was
    // exactly a router arm added without the matching validator-bypass
    // entry (vault.set_secret / vault.delete_secret / vault.list_labels /
    // vault.diagnose / vault.get_session_context / session.reap).
    for builtin in [
        "health.ping",
        "rpc.schemas",
        "session.create",
        "session.list",
        "session.inspect",
        "session.close",
        "session.abort",
        "session.replay",
        "session.diff",
        "session.export",
        "session.validate",
        "session.kill",
        "session.reap",
        "vault.grant",
        "vault.revoke",
        "vault.list_grants",
        "vault.add",
        "vault.set_secret",
        "vault.delete_secret",
        "vault.list_labels",
        "vault.diagnose",
        "vault.get_session_context",
        "content.get",
        "gc.run",
        "import.playwright",
        "daemon.health",
        "request.cancel",
    ] {
        let outcome = validator.validate_request(builtin, &json!({}));
        assert!(
            matches!(outcome, ValidationOutcome::Pass),
            "built-in core method {builtin} must pass validation; got {outcome:?}"
        );
    }
}

/// CLASS-KILLER for the `method_not_found`-on-installed-daemons regression
/// family (audit 2026-06-10 / `session.reap` feature spec, AC 2).
///
/// The hand-maintained list in the test above goes stale the same way
/// `BUILTIN_CORE_METHODS` itself went stale twice (`session.create` post-#55,
/// then vault.set_secret/.../session.reap): someone adds a `RequestRouter`
/// dispatch arm and forgets the validator-bypass entry, and nothing fails
/// until an installed daemon (with postinstall web.* schemas loaded) rejects
/// the new method on the wire.
///
/// This test derives the routed-method set FROM THE ROUTER SOURCE instead of
/// a copied list: it scans `src/request_router/mod.rs` for match arms whose
/// pattern is a quoted `<surface>.<verb>` literal, drops the schema-driven
/// `web.*` surface verbs, and asserts every remaining routed method passes
/// `validate_request` against a POPULATED registry (one unrelated web schema,
/// mirroring a postinstalled daemon — the empty-registry bypass must not be
/// what saves these methods). A new router arm without a matching
/// `BUILTIN_CORE_METHODS` entry now fails HERE, at PR time.
#[test]
fn every_routed_core_method_passes_validation_with_populated_registry() {
    let router_src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/request_router/mod.rs"
    ))
    .expect("request_router/mod.rs must be readable from the crate root");

    fn is_method_shape(s: &str) -> bool {
        // exactly one '.', non-empty [a-z_]+ segments on both sides
        let mut parts = s.split('.');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(a), Some(b), None) => {
                !a.is_empty()
                    && !b.is_empty()
                    && a.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                    && b.chars().all(|c| c.is_ascii_lowercase() || c == '_')
            }
            _ => false,
        }
    }

    let mut routed: Vec<String> = router_src
        .lines()
        .filter_map(|line| {
            let t = line.trim_start();
            // Match-arm shape: `"<literal>" => ...` (the pattern is the
            // first token on the line). This deliberately also picks up the
            // `router_required_params` table — every entry there is a routed
            // method too.
            let rest = t.strip_prefix('"')?;
            let (literal, after) = rest.split_once('"')?;
            if !after.trim_start().starts_with("=>") {
                return None;
            }
            if !is_method_shape(literal) || literal.starts_with("web.") {
                return None;
            }
            Some(literal.to_string())
        })
        .collect();
    routed.sort();
    routed.dedup();

    // Extraction-rot guard: the router currently dispatches 20+ core
    // methods; if the source shape changes enough that we extract almost
    // nothing, fail loudly instead of green-washing.
    assert!(
        routed.len() >= 20,
        "extracted only {} routed core methods from request_router/mod.rs — \
         the source scan in this test needs updating: {routed:?}",
        routed.len()
    );
    assert!(
        routed.iter().any(|m| m == "session.reap"),
        "sanity: session.reap must be among the extracted routed methods"
    );

    // Populated registry (NOT empty): the validator's empty-registry bypass
    // must not be what lets these methods through.
    let provider = Arc::new(OneMethodProvider {
        method: "web.click".to_string(),
        request_schema: web_click_schema(),
    });
    let validator = SchemaValidator::new(provider);

    let mut rejected: Vec<String> = Vec::new();
    for method in &routed {
        let outcome = validator.validate_request(method, &json!({}));
        if !matches!(outcome, ValidationOutcome::Pass) {
            rejected.push(format!("{method} -> {outcome:?}"));
        }
    }
    assert!(
        rejected.is_empty(),
        "router-dispatched core methods rejected by the schema validator on a \
         postinstalled daemon (add them to BUILTIN_CORE_METHODS in \
         schema_validator/mod.rs): {rejected:#?}"
    );
}
