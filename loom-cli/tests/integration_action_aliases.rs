//! Integration tests for `cli-route-web-type-text` feature.
//! Covers AC-CLIROUTE-01 .. AC-CLIROUTE-03 + AC-CLIROUTE-05.
//!
//! AC-04 (rpc.schemas advertising aliases) is covered by
//! `loom-rpc/tests/integration_action_routing.rs`.
//!
//! Test pattern mirrors `integration_action_errors.rs::test_ac_aesf_*`:
//! - Build a SchemaCache via `SchemaCache::load` against a temp dir.
//! - Drive `validate_args` directly — the function whose false-negative
//!   surfaced as the Phase 8 dogfood "unknown method: web.type-text" bug.

use loom_cli::action_commands::validate_args;
use loom_cli::error_mapper::{format_error, map_exit_code, CliError, EXIT_USAGE};
use loom_cli::schema_cache::SchemaCache;
use serde_json::json;
use tempfile::TempDir;

/// Build a SchemaCache that mirrors what `loom postinstall` would produce
/// today: `web.type` is the on-disk canonical (renamed from `web.type_text`)
/// with the same property set (`session`, `selector`, `text`).
fn schema_cache_with_web_type() -> (SchemaCache, TempDir) {
    let dir = TempDir::new().unwrap();
    let schema = json!({
        "request": {
            "type": "object",
            "properties": {
                "session":  {"type": "string"},
                "selector": {"type": "string"},
                "text":     {"type": "string"}
            },
            "required": ["session", "selector", "text"]
        },
        "response": {}
    });
    std::fs::write(
        dir.path().join("web.type.json"),
        serde_json::to_string_pretty(&schema).unwrap(),
    )
    .unwrap();
    let cache = SchemaCache::load(dir.path()).unwrap();
    (cache, dir)
}

fn well_formed_args() -> serde_json::Value {
    json!({"session": "01HW", "selector": "input", "text": "x"})
}

// ── AC-CLIROUTE-01: `web.type` accepted ─────────────────────────────────────

#[test]
fn test_ac_cliroute_01_web_type_canonical_accepted() {
    let (schemas, _dir) = schema_cache_with_web_type();
    let args = well_formed_args();
    validate_args(&schemas, "web.type", &args)
        .expect("AC-CLIROUTE-01: web.type must be accepted (no 'unknown method')");
}

// ── AC-CLIROUTE-02: `web.type_text` continues to work via alias ─────────────

#[test]
fn test_ac_cliroute_02_web_type_text_alias_accepted() {
    let (schemas, _dir) = schema_cache_with_web_type();
    let args = well_formed_args();
    validate_args(&schemas, "web.type_text", &args)
        .expect("AC-CLIROUTE-02: web.type_text must be accepted as an alias for web.type");
}

// ── AC-CLIROUTE-03: `web.type-text` rejected with hint ──────────────────────

#[test]
fn test_ac_cliroute_03_web_type_kebab_rejected_with_hint_for_canonical() {
    let (schemas, _dir) = schema_cache_with_web_type();
    let args = well_formed_args();
    let err = validate_args(&schemas, "web.type-text", &args)
        .expect_err("AC-CLIROUTE-03: web.type-text must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("web.type-text"),
        "error must name the rejected method; got: {msg}"
    );
    assert!(
        msg.contains("web.type"),
        "error must hint at the canonical 'web.type'; got: {msg}"
    );
}

#[test]
fn test_ac_cliroute_03_web_type_kebab_rejected_with_hint_for_alias() {
    let (schemas, _dir) = schema_cache_with_web_type();
    let args = well_formed_args();
    let err = validate_args(&schemas, "web.type-text", &args)
        .expect_err("AC-CLIROUTE-03: web.type-text must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("web.type_text"),
        "error must hint at the legacy alias 'web.type_text' too; got: {msg}"
    );
}

#[test]
fn test_ac_cliroute_03_kebab_rejection_exit_code_is_2() {
    let (schemas, _dir) = schema_cache_with_web_type();
    let args = well_formed_args();
    let r: Result<(), CliError> = Err(validate_args(&schemas, "web.type-text", &args).unwrap_err());
    assert_eq!(map_exit_code(&r), EXIT_USAGE);
    let msg = format_error(&r).unwrap();
    assert!(
        !msg.is_empty(),
        "format_error for kebab rejection must be non-empty"
    );
}

// ── AC-CLIROUTE-03 negative control: still errors on a totally-unrelated method

#[test]
fn test_ac_cliroute_03_unknown_unrelated_method_still_errors() {
    let (schemas, _dir) = schema_cache_with_web_type();
    let args = well_formed_args();
    let err = validate_args(&schemas, "bogus.method", &args)
        .expect_err("unrelated unknown methods must still error");
    let msg = err.to_string();
    assert!(msg.contains("bogus.method"), "got: {msg}");
}

// ── AC-CLIROUTE-05: every alias target exists in BUILTIN_SCHEMAS ────────────
// Drift guard: if a future contributor renames the on-disk canonical, the
// alias table will refer to a missing schema. Catch it here, not at runtime.

#[test]
fn test_ac_cliroute_05_every_alias_target_appears_in_builtin_schemas() {
    use loom_cli::postinstall_runner::BUILTIN_SCHEMAS;
    use loom_shared::action_aliases::METHOD_ALIASES;
    let canonicals: std::collections::HashSet<&str> =
        BUILTIN_SCHEMAS.iter().map(|(m, _)| *m).collect();
    for (alias, canonical) in METHOD_ALIASES {
        assert!(
            canonicals.contains(canonical),
            "alias table references canonical method '{canonical}' \
             (for alias '{alias}') but BUILTIN_SCHEMAS has no schema for it"
        );
    }
}
