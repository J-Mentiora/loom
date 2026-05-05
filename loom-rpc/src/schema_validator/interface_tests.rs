// Interface tests for `SchemaValidator`. Verifies pre-dispatch
// position, strict-mode violation kinds, response-side
// validation for.

use super::schema_validator::{
    SchemaValidator, SchemaValidatorApi, ValidationOutcome, ViolationKind,
};
use crate::error_translator::error_translator::JsonRpcError;
use crate::schema_provider::schema_provider::SchemaProviderApi;
use std::sync::Arc;

// Compile-only fake provider — confirms validator's constructor
// accepts `Arc<dyn SchemaProviderApi>`.
struct _FakeProvider;
impl SchemaProviderApi for _FakeProvider {
    fn lookup_request_schema(
        &self,
        _method: &str,
    ) -> Option<Arc<crate::schema_provider::schema_provider::CompiledJsonSchema>> {
        None
    }
    fn lookup_response_schema(
        &self,
        _method: &str,
    ) -> Option<Arc<crate::schema_provider::schema_provider::CompiledJsonSchema>> {
        None
    }
    fn registered_methods(&self) -> Vec<String> {
        vec![]
    }
    fn get_registry_snapshot(&self) -> crate::schema_provider::schema_provider::SchemaRegistry {
        crate::schema_provider::schema_provider::SchemaRegistry {
            methods: vec![],
            source_wit_sha256: "0".into(),
        }
    }
}

#[test]
fn validator_constructor_takes_arc_provider() {
    fn _ck(p: Arc<dyn SchemaProviderApi>) -> Arc<SchemaValidator> {
        SchemaValidator::new(p)
    }
    let _ = _ck;
}

#[test]
fn validate_request_signature_takes_method_and_params() {
    fn _ck<V: SchemaValidatorApi>(v: &V, m: &str, p: &serde_json::Value) -> ValidationOutcome {
        v.validate_request(m, p)
    }
    let _ = _ck::<SchemaValidator>;
}

#[test]
fn validate_response_signature_for_ic_rpc_10() {
    // vault.grant response stripped of secret-shaped fields.
    fn _ck<V: SchemaValidatorApi>(v: &V, m: &str, r: &serde_json::Value) -> ValidationOutcome {
        v.validate_response(m, r)
    }
    let _ = _ck::<SchemaValidator>;
}

#[test]
fn validation_outcome_distinguishes_pass_violation_method_not_found() {
    // failure short-circuits dispatch.
    let _: ValidationOutcome = ValidationOutcome::Pass;
    fn _ck_v(e: JsonRpcError) -> ValidationOutcome {
        ValidationOutcome::Violation(e)
    }
    fn _ck_m(e: JsonRpcError) -> ValidationOutcome {
        ValidationOutcome::MethodNotFound(e)
    }
    let _ = _ck_v;
    let _ = _ck_m;
}

#[test]
fn violation_kind_covers_sr_rpc_02_categories() {
    // strict mode coverage.
    let _ = ViolationKind::FieldMissing;
    let _ = ViolationKind::FieldUnknown;
    let _ = ViolationKind::TypeMismatch;
    let _ = ViolationKind::EnumViolation;
}

#[test]
fn violation_kind_emits_canonical_expected_strings() {
    // Wire stability for `SchemaViolationDetail.expected`.
    assert_eq!(
        ViolationKind::FieldMissing.as_expected_str(),
        "field_missing"
    );
    assert_eq!(
        ViolationKind::FieldUnknown.as_expected_str(),
        "field_unknown"
    );
    assert_eq!(
        ViolationKind::TypeMismatch.as_expected_str(),
        "type_mismatch"
    );
    assert_eq!(
        ViolationKind::EnumViolation.as_expected_str(),
        "enum_violation"
    );
}
