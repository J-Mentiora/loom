//! Integration tests for `rpc-schemas-method-missing` feature.
//! Covers AC-PROTO-02.2 from the CLI side: `loom action rpc.schemas`
//! must reach the daemon, not be rejected at the CLI's own
//! `validate_args` gate.
//!
//! Wire-side coverage (request_router dispatch, handler returning a
//! SchemaRegistry-shaped envelope) lives in
//! `loom-rpc/tests/integration_action_routing.rs::test_rpc_schemas_*`.
//! This file pins the *CLI-side* contract that the original
//! Phase 8 dogfood bug surfaced ('unknown method: rpc.schemas').

use loom_cli::action_commands::validate_args;
use loom_cli::postinstall_runner::postinstall_runner::BUILTIN_SCHEMAS;
use loom_cli::schema_cache::SchemaCache;
use serde_json::json;
use tempfile::TempDir;

/// Build a SchemaCache populated from `BUILTIN_SCHEMAS` — the same
/// payload `loom postinstall` writes to disk and `SchemaCache::load`
/// rehydrates at CLI startup.
fn schema_cache_from_builtins() -> (SchemaCache, TempDir) {
    let dir = TempDir::new().unwrap();
    for (method, json_str) in BUILTIN_SCHEMAS {
        let parsed: serde_json::Value =
            serde_json::from_str(json_str).expect("BUILTIN_SCHEMAS entries must parse as JSON");
        std::fs::write(
            dir.path().join(format!("{method}.json")),
            serde_json::to_string_pretty(&parsed).unwrap(),
        )
        .unwrap();
    }
    let cache = SchemaCache::load(dir.path()).unwrap();
    (cache, dir)
}

// ── AC-PROTO-02.2: `loom action rpc.schemas` reaches the wire ───────────────

/// Phase 8 dogfood: `loom action rpc.schemas --session $S` returned
/// 'unknown method: rpc.schemas. Available: web.click, web.evaluate, …'.
/// The daemon-side handler exists and works (pinned by
/// `rpc_schemas_returns_per_method_schemas`); the gap was the CLI's
/// `validate_args` rejecting the method before the wire call. This
/// test pins the fix.
#[test]
fn test_ac_proto_02_2_rpc_schemas_passes_cli_validate_args() {
    let (schemas, _dir) = schema_cache_from_builtins();
    // `action_commands::dispatch` always inserts `session` into the
    // params object before calling `validate_args`. We mirror that
    // shape here so the test pins the realistic CLI dispatch path.
    let args = json!({"session": "01HW"});
    validate_args(&schemas, "rpc.schemas", &args).expect(
        "AC-PROTO-02.2: rpc.schemas must pass CLI validate_args so the \
         wire-level dispatch can return the SchemaRegistry. Got an error \
         instead — the CLI is rejecting an introspection method that the \
         daemon supports.",
    );
}

#[test]
fn test_rpc_schemas_listed_in_cli_known_methods() {
    let (schemas, _dir) = schema_cache_from_builtins();
    let methods: Vec<&str> = schemas.methods().collect();
    assert!(
        methods.contains(&"rpc.schemas"),
        "rpc.schemas must appear in CLI known-methods list (used by the \
         'unknown method: X. Available: …' error message); got: {methods:?}"
    );
}
