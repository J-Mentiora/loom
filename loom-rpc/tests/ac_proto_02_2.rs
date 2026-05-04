//! AC-PROTO-02.2 — `rpc.schemas` introspection.
//!
//! Given the server,
//! When the client invokes `rpc.schemas`,
//! Then the response contains a JSON Schema draft-2020-12 entry for
//! every method advertised in the runtime's manifest, with
//! `request_schema` and `response_schema` keys.

use loom_rpc::schema_provider::schema_provider::{SchemaProvider, SchemaProviderApi};
use serde_json::json;
use std::path::PathBuf;
use tempfile::tempdir;

/// Create a temp schema directory with N method JSON files.
/// Each file is named `<method>.json` and contains
/// `{"request": {...}, "response": {...}}`.
fn setup_schema_dir(
    methods: &[(&str, serde_json::Value, serde_json::Value)],
) -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    for (method, req, resp) in methods {
        let file = dir.path().join(format!("{}.json", method));
        let content = json!({
            "method": method,
            "request": req,
            "response": resp,
        });
        std::fs::write(file, serde_json::to_string(&content).unwrap()).unwrap();
    }
    dir
}

#[test]
fn rpc_schemas_returns_per_method_schemas() {
    let dir = setup_schema_dir(&[
        (
            "session.create",
            json!({"type": "object", "properties": {"profile": {"type": "string"}}}),
            json!({"type": "object", "properties": {"session_id": {"type": "string"}}}),
        ),
        (
            "rpc.schemas",
            json!({"type": "object"}),
            json!({"type": "object", "properties": {"methods": {"type": "array"}}}),
        ),
    ]);

    let provider =
        SchemaProvider::load_at_startup(&dir.path().to_path_buf()).expect("load must succeed");

    let snapshot = provider.get_registry_snapshot();

    assert!(
        !snapshot.methods.is_empty(),
        "AC-PROTO-02.2: registry must have at least one method"
    );

    // Every method must have request_schema and response_schema.
    for entry in &snapshot.methods {
        assert!(
            entry.request.is_object() || entry.request.is_array(),
            "AC-PROTO-02.2: method '{}' must have a request schema",
            entry.method
        );
        assert!(
            entry.response.is_object() || entry.response.is_array(),
            "AC-PROTO-02.2: method '{}' must have a response schema",
            entry.method
        );
    }

    // Methods must be sorted by name (BC §3 deterministic canonical-JSON).
    let names: Vec<&str> = snapshot.methods.iter().map(|m| m.method.as_str()).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "AC-PROTO-02.2: methods must be sorted by name");

    // Verify session.create is present.
    assert!(
        snapshot.methods.iter().any(|m| m.method == "session.create"),
        "AC-PROTO-02.2: session.create must appear in registry"
    );
}

#[test]
fn load_at_startup_fails_on_missing_directory() {
    use loom_rpc::schema_provider::schema_provider::SchemaLoadError;
    let result = SchemaProvider::load_at_startup(&PathBuf::from("/no/such/schema/dir"));
    assert!(
        matches!(result, Err(SchemaLoadError::DirectoryMissing { .. })),
        "load_at_startup must return DirectoryMissing for a nonexistent dir"
    );
}

#[test]
fn lookup_request_schema_returns_correct_entry() {
    let dir = setup_schema_dir(&[(
        "session.create",
        json!({"type": "object"}),
        json!({"type": "object"}),
    )]);
    let provider = SchemaProvider::load_at_startup(&dir.path().to_path_buf()).unwrap();
    let schema = provider.lookup_request_schema("session.create");
    assert!(schema.is_some(), "lookup_request_schema must find session.create");
    let missing = provider.lookup_request_schema("not.a.method");
    assert!(missing.is_none(), "lookup_request_schema must return None for unknown methods");
}

#[test]
fn registered_methods_returns_all_methods() {
    let dir = setup_schema_dir(&[
        ("a.method", json!({}), json!({})),
        ("b.method", json!({}), json!({})),
    ]);
    let provider = SchemaProvider::load_at_startup(&dir.path().to_path_buf()).unwrap();
    let mut methods = provider.registered_methods();
    methods.sort();
    assert_eq!(methods, vec!["a.method", "b.method"]);
}
