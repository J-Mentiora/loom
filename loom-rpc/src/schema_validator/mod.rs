//! `schema_validator` — see crate root.
pub mod schema_validator;
pub use schema_validator::*;

#[cfg(test)]
mod interface_tests;

use crate::error_translator::error_translator::{
    ErrorTranslator, LoomErrorCode, SchemaViolationDetail,
};
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
        method: &str,
        schema: &CompiledJsonSchema,
        instance: &serde_json::Value,
    ) -> Option<SchemaViolationDetail> {
        // The validator was compiled exactly once, at schema-load time
        // (`CompiledJsonSchema::compile`). An uncompilable schema can
        // never reach this hot path: the schema providers fail CLOSED
        // with `SchemaLoadError::InvalidSchema` — the old per-request
        // `validator_for(..).ok()?` here mapped a compile failure to
        // `None` (= Pass), silently disabling validation.
        if let Err(err) = schema.validator.validate(instance) {
            return Some(violation_detail(method, schema, &err));
        }
        None
    }
}

/// Build the wire detail for one `jsonschema` violation, naming the ACTUAL
/// offending field. The v0.11.0 bug: root-level violations have an EMPTY
/// instance path, so the old `build_field_path` fallback blamed the literal
/// envelope word "params" and dropped the unexpected/missing property names
/// the error kind carries — producing `field 'params' expected field_unknown
/// got object` for a request whose offending key was `until`.
fn violation_detail(
    method: &str,
    schema: &CompiledJsonSchema,
    err: &jsonschema::ValidationError<'_>,
) -> SchemaViolationDetail {
    use jsonschema::error::{TypeKind, ValidationErrorKind};

    let actual = json_type_name(err.instance().as_ref());
    let base = SchemaViolationDetail {
        field: String::new(),
        expected: String::new(),
        actual,
        method: method.to_string(),
        allowed: None,
    };

    match err.kind() {
        ValidationErrorKind::AdditionalProperties { unexpected } => SchemaViolationDetail {
            field: unexpected.join(", "),
            expected: ViolationKind::FieldUnknown.as_expected_str().to_string(),
            allowed: allowed_properties(schema),
            ..base
        },
        ValidationErrorKind::Required { property } => SchemaViolationDetail {
            field: property
                .as_str()
                .map(|s| s.to_string())
                .unwrap_or_else(|| property.to_string()),
            expected: ViolationKind::FieldMissing.as_expected_str().to_string(),
            ..base
        },
        ValidationErrorKind::Type { kind } => SchemaViolationDetail {
            field: build_field_path(err.instance_path()),
            expected: match kind {
                TypeKind::Single(jt) => jt.as_str().to_string(),
                TypeKind::Multiple(_) => "one of valid types".to_string(),
            },
            ..base
        },
        ValidationErrorKind::Enum { .. } => SchemaViolationDetail {
            field: build_field_path(err.instance_path()),
            expected: ViolationKind::EnumViolation.as_expected_str().to_string(),
            ..base
        },
        _ => SchemaViolationDetail {
            field: build_field_path(err.instance_path()),
            expected: "unknown".to_string(),
            ..base
        },
    }
}

/// The schema's permitted property names, sorted — included in
/// `field_unknown` details so the consumer sees the valid surface inline.
fn allowed_properties(schema: &CompiledJsonSchema) -> Option<Vec<String>> {
    let props = schema.inner.get("properties")?.as_object()?;
    let mut names: Vec<String> = props.keys().cloned().collect();
    names.sort();
    Some(names)
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
        // dispatch-level canonicalisation would never fire .
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

        match self.first_violation(method, &schema, params) {
            Some(detail) => {
                let err = ErrorTranslator::from_schema_violation(detail);
                ValidationOutcome::Violation(err)
            }
            None => ValidationOutcome::Pass,
        }
    }

    fn validate_response(&self, method: &str, response: &serde_json::Value) -> ValidationOutcome {
        // Mirror request-side canonicalisation so a response from an
        // alias-spelled method finds its schema .
        let method = loom_shared::action_aliases::canonicalise(method);
        let schema = match self.provider.lookup_response_schema(method) {
            Some(s) => s,
            None => return ValidationOutcome::Pass, // No response schema = pass
        };

        match self.first_violation(method, &schema, response) {
            Some(detail) => {
                let err = ErrorTranslator::from_schema_violation(detail);
                ValidationOutcome::Violation(err)
            }
            None => ValidationOutcome::Pass,
        }
    }
}
