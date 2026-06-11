// Interface tests for `SchemaProvider`. Verifies startup
// load, in-memory snapshot, and WIT-source-of-truth invariants.

use super::schema_provider::{
    CompiledJsonSchema, MethodSchema, SchemaLoadError, SchemaProvider, SchemaProviderApi,
    SchemaRegistry,
};
use std::path::PathBuf;
use std::sync::Arc;

#[test]
fn schema_registry_has_methods_and_source_wit_sha256_fields() {
    fn _ck(r: &SchemaRegistry) {
        let _: &Vec<MethodSchema> = &r.methods;
        let _: &String = &r.source_wit_sha256;
    }
    let _ = _ck;
}

#[test]
fn method_schema_carries_method_request_response() {
    fn _ck(m: &MethodSchema) {
        let _: &String = &m.method;
        let _: &serde_json::Value = &m.request;
        let _: &serde_json::Value = &m.response;
    }
    let _ = _ck;
}

#[test]
fn load_at_startup_takes_path_returns_arc_provider() {
    fn _ck(p: &PathBuf) -> Result<Arc<SchemaProvider>, SchemaLoadError> {
        SchemaProvider::load_at_startup(p)
    }
    let _ = _ck;
}

#[test]
fn schema_load_error_distinguishes_missing_dir_invalid_schema_empty() {
    // Daemon refuses to start if dir is missing or any
    // expected file is absent.
    let _ = SchemaLoadError::DirectoryMissing {
        path: PathBuf::from("/x"),
    };
    let _ = SchemaLoadError::InvalidSchema {
        method: "session.create".into(),
        reason: "syntax".into(),
    };
    let _ = SchemaLoadError::EmptyDirectory {
        path: PathBuf::from("/x"),
    };
}

#[test]
fn lookup_request_schema_signature() {
    fn _ck<P: SchemaProviderApi>(p: &P, method: &str) -> Option<Arc<CompiledJsonSchema>> {
        p.lookup_request_schema(method)
    }
    let _ = _ck::<SchemaProvider>;
}

#[test]
fn lookup_response_schema_signature() {
    // response-side validation for vault.grant.
    fn _ck<P: SchemaProviderApi>(p: &P, method: &str) -> Option<Arc<CompiledJsonSchema>> {
        p.lookup_response_schema(method)
    }
    let _ = _ck::<SchemaProvider>;
}

#[test]
fn registered_methods_returns_method_names_for_router_enumeration() {
    // RequestRouter walks this at startup.
    fn _ck<P: SchemaProviderApi>(p: &P) -> Vec<String> {
        p.registered_methods()
    }
    let _ = _ck::<SchemaProvider>;
}

#[test]
fn get_registry_snapshot_returns_in_memory_snapshot_no_disk_read() {
    // In-memory only.
    fn _ck<P: SchemaProviderApi>(p: &P) -> SchemaRegistry {
        p.get_registry_snapshot()
    }
    let _ = _ck::<SchemaProvider>;
}

#[test]
fn compiled_json_schema_exposes_underlying_json_for_snapshot() {
    fn _ck(s: &CompiledJsonSchema) -> &serde_json::Value {
        s.as_json()
    }
    let _ = _ck;
}

#[test]
fn compile_rejects_invalid_schema_document() {
    // `{"type": 123}` is valid JSON but not a valid JSON Schema.
    assert!(CompiledJsonSchema::compile(serde_json::json!({"type": 123})).is_err());
}

/// Fail-closed regression: a stored schema that does not
/// compile must fail the whole load with a typed error — it must NOT load
/// and later pass every request unvalidated (the old per-request
/// `validator_for(..).ok()?` fail-open path).
#[test]
fn load_at_startup_fails_closed_when_a_schema_does_not_compile() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("web.navigate.json"),
        r#"{"request":{"type":123},"response":{"type":"object"}}"#,
    )
    .unwrap();

    let err = SchemaProvider::load_at_startup(&dir.path().to_path_buf())
        .err()
        .expect("uncompilable schema must fail the load");
    match err {
        SchemaLoadError::InvalidSchema { method, reason } => {
            assert_eq!(method, "web.navigate");
            assert!(
                reason.contains("request"),
                "reason should name the failing side: {reason}"
            );
        }
        other => panic!("expected InvalidSchema, got {other:?}"),
    }
}

/// `source_wit_sha256` regression: must be a real SHA-256
/// (64 lowercase hex chars) of the schema file bytes concatenated in
/// path-sorted order — NOT a 16-hex DefaultHasher over read_dir order.
/// The expected digest is hardcoded so any algorithm/order drift fails.
#[test]
fn source_wit_sha256_is_real_sha256_of_path_sorted_file_bytes() {
    let click = r#"{"request":{"type":"object","additionalProperties":false},"response":{"type":"object"}}"#;
    let typ = r#"{"request":{"type":"object"},"response":{"type":"object"}}"#;

    let dir = tempfile::tempdir().unwrap();
    // Written in reverse-alphabetical order: sorting (not creation or
    // read_dir order) must drive the hash.
    std::fs::write(dir.path().join("web.type.json"), typ).unwrap();
    std::fs::write(dir.path().join("web.click.json"), click).unwrap();

    let provider = SchemaProvider::load_at_startup(&dir.path().to_path_buf()).unwrap();
    let got = provider.get_registry_snapshot().source_wit_sha256;

    // shasum -a 256 of click-bytes ++ type-bytes (sorted file order).
    assert_eq!(
        got,
        "6cdff6c7a22cb422ec092b54dc63f1c71504a3b0a981c97853bca90e547c54a0"
    );
    assert_eq!(got.len(), 64, "must be full-width sha256 hex");
}

/// The validator is compiled at load time and is immediately usable —
/// no per-request recompilation, no fail-open.
#[test]
fn load_at_startup_precompiles_validators() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("web.click.json"),
        r#"{"request":{"type":"object","properties":{"selector":{"type":"string"}},"required":["selector"],"additionalProperties":false},"response":{"type":"object"}}"#,
    )
    .unwrap();

    let provider = SchemaProvider::load_at_startup(&dir.path().to_path_buf()).unwrap();
    let schema = provider
        .lookup_request_schema("web.click")
        .expect("web.click must be registered");
    // Precompiled validator enforces the schema (in-crate access).
    assert!(schema
        .validator
        .validate(&serde_json::json!({"selector": 42}))
        .is_err());
    assert!(schema
        .validator
        .validate(&serde_json::json!({"selector": "#go"}))
        .is_ok());
}
