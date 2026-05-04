// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-rpc/modules/schema_provider/interface_tests.rs` instead.
// Interface tests for `SchemaProvider`. Verifies IC-RPC-01 startup
// load, IC-RPC-02 in-memory snapshot, BC-RPC-02 WIT-source-of-truth.

use super::schema_provider::{
    CompiledJsonSchema, MethodSchema, SchemaLoadError, SchemaProvider,
    SchemaProviderApi, SchemaRegistry,
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
    // IC-RPC-01: daemon refuses to start if dir is missing or any
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
    fn _ck<P: SchemaProviderApi>(
        p: &P,
        method: &str,
    ) -> Option<Arc<CompiledJsonSchema>> {
        p.lookup_request_schema(method)
    }
    let _ = _ck::<SchemaProvider>;
}

#[test]
fn lookup_response_schema_signature() {
    // IC-RPC-10: response-side validation for vault.grant.
    fn _ck<P: SchemaProviderApi>(
        p: &P,
        method: &str,
    ) -> Option<Arc<CompiledJsonSchema>> {
        p.lookup_response_schema(method)
    }
    let _ = _ck::<SchemaProvider>;
}

#[test]
fn registered_methods_returns_method_names_for_router_enumeration() {
    // SR-RPC-03: RequestRouter walks this at startup.
    fn _ck<P: SchemaProviderApi>(p: &P) -> Vec<String> {
        p.registered_methods()
    }
    let _ = _ck::<SchemaProvider>;
}

#[test]
fn get_registry_snapshot_returns_in_memory_snapshot_no_disk_read() {
    // IC-RPC-02: in-memory only.
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
