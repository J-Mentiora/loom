// Re-export of the locked v5.3 interface. DO NOT EDIT here.
// Edit `systems/loom-rpc/modules/schema_validator/interfaces.rs` instead.
// SchemaValidator — pre-dispatch JSON Schema validation as a
// `jsonrpsee` request-extension middleware.
//
// # Contract semantics
// - **Pre-dispatch .** Runs as a `jsonrpsee` middleware
//   layer that wraps `RpcModule`'s call handler. Validation executes
//   BEFORE dispatch; failures short-circuit with
//   `LoomErrorCode::SchemaViolation` carrying `{field, expected, actual}`.
// - **Strict mode .** Schemas have
//   `additionalProperties: false`. Coverage: missing required field →
//   `field_missing`; unknown field → `field_unknown`; wrong type →
//   `type_mismatch`; out-of-enum → `enum_violation`. The first
//   violation observed is the one returned (strict-mode deterministic).
// - **Source of truth .** Validator reads compiled schemas
//   from `SchemaProvider` only — never compiles its own.
// - **Method-not-found.** If `SchemaProvider::lookup_request_schema`
//   returns `None`, validator emits `LoomErrorCode::MethodNotFound`.

use crate::error_translator::error_translator::JsonRpcError;
use crate::schema_provider::schema_provider::SchemaProviderApi;
use std::sync::Arc;

/// Outcome of validating one `(method, params)` pair.
#[derive(Debug)]
pub enum ValidationOutcome {
    /// Params satisfied the schema. Continue to dispatch.
    Pass,
    /// Schema violation. Return the envelope as-is (already
    /// constructed by `ErrorTranslator::from_schema_violation`).
    Violation(JsonRpcError),
    /// Method not in registry. Return MethodNotFound envelope.
    MethodNotFound(JsonRpcError),
}

/// Trait surface for the validator so a fake can be substituted in
/// `RpcHandlers` integration tests.
pub trait SchemaValidatorApi: Send + Sync {
    /// Validate request `params` against the registered schema for
    /// `method`. Returns `Pass` on success; `Violation` on schema
    /// failure; `MethodNotFound` if the method is not registered.
    fn validate_request(&self, method: &str, params: &serde_json::Value) -> ValidationOutcome;

    /// Validate a response payload against the registered response
    /// schema. Used by `RpcHandlers::vault_grant` to assert
    /// (no `secret`/`token`/`value` fields ever leak in the response).
    fn validate_response(&self, method: &str, response: &serde_json::Value) -> ValidationOutcome;
}

pub struct SchemaValidator {
    pub(crate) provider: Arc<dyn SchemaProviderApi>,
}

/// Strict-mode violation kinds . Wire-equal to the
/// `expected` field convention in `SchemaViolationDetail`. The validator
/// picks the most-specific match per the JSON Schema draft-2020-12
/// algorithm (errors listed in document order, first-fail-fast).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViolationKind {
    /// Required property absent.
    FieldMissing,
    /// Property present but not declared in the schema (strict mode).
    FieldUnknown,
    /// Type does not match the schema's declared type.
    TypeMismatch,
    /// Value outside the declared `enum` list.
    EnumViolation,
}

impl ViolationKind {
    /// Return the canonical `expected` string used in
    /// `SchemaViolationDetail.expected` so the wire shape is stable.
    pub fn as_expected_str(&self) -> &'static str {
        match self {
            ViolationKind::FieldMissing => "field_missing",
            ViolationKind::FieldUnknown => "field_unknown",
            ViolationKind::TypeMismatch => "type_mismatch",
            ViolationKind::EnumViolation => "enum_violation",
        }
    }
}
