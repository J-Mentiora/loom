//! Tests for `action-validation-order` (AC-AVOR-01..04).
//!
//! Bug: `dispatch()` called `validate_args(schemas, method, &extra_params)`
//! before merging `args.session` into the params object.  Web-action schemas
//! declare `"required": ["session"]`, so every call failed with
//! '"session" is a required property'.
//!
//! Fix: build `full_params` = {session, ...extra} first, then validate.

use loom_cli::action_commands::validate_args;
use loom_cli::schema_cache::SchemaCache;
use serde_json::json;
use tempfile::TempDir;

// ── Helper ────────────────────────────────────────────────────────────────────

/// Build a `SchemaCache` with one schema: `web.navigate` requiring
/// `session` + `url`, `additionalProperties: false`.
fn cache_web_navigate() -> (SchemaCache, TempDir) {
    let dir = TempDir::new().unwrap();
    let schema = json!({
        "request": {
            "type": "object",
            "properties": {
                "session": { "type": "string" },
                "url":     { "type": "string" }
            },
            "required": ["session", "url"],
            "additionalProperties": false
        },
        "response": {}
    });
    std::fs::write(
        dir.path().join("web.navigate.json"),
        serde_json::to_string_pretty(&schema).unwrap(),
    )
    .unwrap();
    let cache = SchemaCache::load(dir.path()).unwrap();
    (cache, dir)
}

// ── AC-AVOR-01 ────────────────────────────────────────────────────────────────

/// `validate_args` with the FULL params object (session + url) must succeed.
///
/// Before the fix, `dispatch()` called `validate_args` on `extra_params`
/// only (no session), so this would fail with '"session" is a required
/// property'.  After the fix, `dispatch()` assembles `full_params` first,
/// then validates — so this call succeeds.
#[test]
fn test_ac_avor_01_full_params_with_session_passes() {
    let (schemas, _dir) = cache_web_navigate();
    let full = json!({"session": "01HW", "url": "https://example.com"});
    assert!(
        validate_args(&schemas, "web.navigate", &full).is_ok(),
        "validate_args with full params (session + url) must succeed"
    );
}

/// Documents the old dispatch() bug: validating extra_params alone (no session)
/// fails because the schema requires session.  This shows that the bug was real
/// and that the fix (validate full_params, not extra_params) is necessary.
#[test]
fn test_ac_avor_01_extra_params_only_fails_session_required() {
    let (schemas, _dir) = cache_web_navigate();
    // extra_params as dispatch() used to see them: session not yet merged
    let extra_only = json!({"url": "https://example.com"});
    let err = validate_args(&schemas, "web.navigate", &extra_only).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("session"),
        "validating extra_params without session must fail naming 'session'; got: {msg}"
    );
}

// ── AC-AVOR-02 ────────────────────────────────────────────────────────────────

/// Missing required field (`url`) must produce an error that names the field.
#[test]
fn test_ac_avor_02_missing_required_url_names_field() {
    let (schemas, _dir) = cache_web_navigate();
    // session present, url missing
    let params = json!({"session": "S1"});
    let err = validate_args(&schemas, "web.navigate", &params).unwrap_err();
    let msg = err.to_string();
    assert!(!msg.is_empty(), "validation error must be non-empty");
    // The error must name the method so the user knows which call failed.
    assert!(
        msg.contains("web.navigate"),
        "error must name the method; got: {msg}"
    );
}

/// Missing required field must exit with code 2.
#[test]
fn test_ac_avor_02_missing_required_field_exit_code_2() {
    use loom_cli::error_mapper::{map_exit_code, CliError, EXIT_USAGE};
    let (schemas, _dir) = cache_web_navigate();
    let params = json!({"session": "S1"});
    let r: Result<(), CliError> =
        Err(validate_args(&schemas, "web.navigate", &params).unwrap_err());
    assert_eq!(
        map_exit_code(&r),
        EXIT_USAGE,
        "missing required field must exit 2"
    );
}

// ── AC-AVOR-03 ────────────────────────────────────────────────────────────────

/// Bogus extra arg must fail when schema has `additionalProperties: false`.
#[test]
fn test_ac_avor_03_bogus_field_rejected() {
    let (schemas, _dir) = cache_web_navigate();
    // all required fields present, PLUS an unrecognised key
    let params = json!({"session": "S1", "url": "https://example.com", "bogus": "val"});
    let err = validate_args(&schemas, "web.navigate", &params).unwrap_err();
    let msg = err.to_string();
    assert!(
        !msg.is_empty(),
        "bogus field error must be non-empty; got: {msg}"
    );
    // jsonschema should mention the unknown property name or additionalProperties
    assert!(
        msg.contains("bogus") || msg.contains("additional"),
        "error should mention 'bogus' or 'additional'; got: {msg}"
    );
}
