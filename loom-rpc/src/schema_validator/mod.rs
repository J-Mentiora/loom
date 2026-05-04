//! `schema_validator` — see `systems/loom-rpc/modules/schema_validator/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod schema_validator;
pub use schema_validator::*;

#[cfg(test)]
mod interface_tests;

use crate::error_translator::error_translator::{ErrorTranslator, LoomErrorCode, SchemaViolationDetail};
use crate::schema_provider::schema_provider::{CompiledJsonSchema, SchemaProviderApi};
use std::sync::Arc;

/// Built-in core RPC methods handled by hand-written paths in
/// `RequestRouter::dispatch` + `RpcHandlers`. These have no JSON Schema
/// in the schema_provider registry (the provider only loads per-action
/// web.* / shell.* / etc. schemas from disk at postinstall time).
///
/// The validator must pass these through without consulting the
/// schema registry — otherwise once the registry has been populated
/// with web.* schemas, `registered_methods` becomes non-empty and the
/// validator's no-schema arm rejects every core method as
/// `method_not_found` (see the regression where `loom session create`
/// failed with `method_not_found: session.create` after postinstall
/// loaded the web.* schemas).
const BUILTIN_CORE_METHODS: &[&str] = &[
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
    "vault.grant",
    "vault.revoke",
    "vault.list_grants",
    "vault.add",
    "content.get",
    "gc.run",
    "import.playwright",
];

fn is_builtin_core_method(method: &str) -> bool {
    BUILTIN_CORE_METHODS.contains(&method)
}

impl SchemaValidator {
    pub fn new(provider: Arc<dyn SchemaProviderApi>) -> Arc<Self> {
        Arc::new(Self { provider })
    }

    pub fn first_violation(
        &self,
        schema: &CompiledJsonSchema,
        instance: &serde_json::Value,
    ) -> Option<SchemaViolationDetail> {
        let validator = jsonschema::validator_for(&schema.inner).ok()?;
        if let Err(err) = validator.validate(instance) {
            let field = build_field_path(err.instance_path());
            let (expected, actual) = classify_violation(&err);
            return Some(SchemaViolationDetail { field, expected, actual });
        }
        None
    }
}

/// Build a dot-separated path like "params.selector" from a jsonschema `Location`.
fn build_field_path(path: &jsonschema::paths::Location) -> String {
    let tokens: Vec<String> = path.into_iter().map(|seg| seg.to_string()).collect();
    if tokens.is_empty() {
        "params".to_string()
    } else {
        format!("params.{}", tokens.join("."))
    }
}

/// Map a `ValidationError` to (expected_type_str, actual_type_str).
fn classify_violation(err: &jsonschema::ValidationError<'_>) -> (String, String) {
    use jsonschema::error::{TypeKind, ValidationErrorKind};

    let actual = json_type_name(err.instance().as_ref());

    let expected = match err.kind() {
        ValidationErrorKind::Type { kind } => match kind {
            TypeKind::Single(jt) => jt.as_str().to_string(),
            TypeKind::Multiple(_) => "one of valid types".to_string(),
        },
        ValidationErrorKind::Required { .. } => {
            ViolationKind::FieldMissing.as_expected_str().to_string()
        }
        ValidationErrorKind::AdditionalProperties { .. } => {
            ViolationKind::FieldUnknown.as_expected_str().to_string()
        }
        ValidationErrorKind::Enum { .. } => {
            ViolationKind::EnumViolation.as_expected_str().to_string()
        }
        _ => "unknown".to_string(),
    };

    (expected, actual)
}

/// Return the JSON type name of a `serde_json::Value`.
fn json_type_name(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(n) => {
            if n.is_f64() && !n.as_f64().unwrap().fract().eq(&0.0) {
                "number"
            } else {
                "integer"
            }
        }
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
    .to_string()
}

impl SchemaValidatorApi for SchemaValidator {
    fn validate_request(&self, method: &str, params: &serde_json::Value) -> ValidationOutcome {
        // Canonicalise alias method names before any registry lookup so
        // legacy spellings (e.g. `web.type_text`) resolve to their canonical
        // form (`web.type`) BEFORE we consult `registered_methods` —
        // otherwise validation would reject the alias as MethodNotFound and
        // dispatch-level canonicalisation would never fire (AC-CLIROUTE-02).
        let method = loom_shared::action_aliases::canonicalise(method);

        // Built-in core RPC methods (session.*, vault.*, content.*, gc.*,
        // rpc.*, health.*) are handled by hand-written paths in
        // `RequestRouter` and `RpcHandlers` — they have NO JSON Schema in
        // the schema_provider registry (which only loads the per-action
        // web.*/shell.*/etc. schemas at install time). Bypass schema
        // validation for these so the validator doesn't reject them as
        // `method_not_found` when the provider only has web.* schemas
        // loaded. The router's match arm + RpcHandlers are the
        // authoritative dispatch table for these methods.
        if is_builtin_core_method(method) {
            return ValidationOutcome::Pass;
        }

        let schema = self.provider.lookup_request_schema(method);
        if schema.is_none() {
            // Method unknown to provider: check registered_methods for existence.
            // Built-ins (e.g. rpc.schemas) appear in registered_methods but have
            // no schema — they pass validation with no param check.
            //
            // Empty registry (pre-postinstall / EmptySchemas): bypass all
            // validation so the daemon can serve core methods before schemas are
            // compiled. This matches the daemon startup comment:
            // "schema validation is bypassed (no method schemas = pass)".
            let registered = self.provider.registered_methods();
            if registered.is_empty() {
                return ValidationOutcome::Pass;
            }
            if !registered.contains(&method.to_string()) {
                let err = crate::error_translator::error_translator::JsonRpcError {
                    code: LoomErrorCode::MethodNotFound,
                    message: format!("method not found: {}", method),
                    data: None,
                };
                return ValidationOutcome::MethodNotFound(err);
            }
            return ValidationOutcome::Pass;
        }
        let schema = schema.unwrap();

        match self.first_violation(&schema, params) {
            Some(detail) => {
                let err = ErrorTranslator::from_schema_violation(detail);
                ValidationOutcome::Violation(err)
            }
            None => ValidationOutcome::Pass,
        }
    }

    fn validate_response(&self, method: &str, response: &serde_json::Value) -> ValidationOutcome {
        // Mirror request-side canonicalisation so a response from an
        // alias-spelled method finds its schema (AC-CLIROUTE-02).
        let method = loom_shared::action_aliases::canonicalise(method);
        let schema = match self.provider.lookup_response_schema(method) {
            Some(s) => s,
            None => return ValidationOutcome::Pass, // No response schema = pass
        };

        match self.first_violation(&schema, response) {
            Some(detail) => {
                let err = ErrorTranslator::from_schema_violation(detail);
                ValidationOutcome::Violation(err)
            }
            None => ValidationOutcome::Pass,
        }
    }
}
