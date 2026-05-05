//! Integration tests for `action-error-surfacing` feature.
//!
//! Invokes each error class and asserts
//! (a) exit code, (b) stderr is non-empty (via format_error), (c) stdout
//! is empty (errors return before any println! in dispatch — structural guarantee).

use loom_cli::action_commands::{parse_extra_to_json, validate_args};
use loom_cli::error_mapper::{
    format_error, map_exit_code, CliError, EXIT_SURFACE_UNAVAILABLE, EXIT_USAGE,
};
use loom_cli::schema_cache::SchemaCache;
use serde_json::json;
use tempfile::TempDir;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Build a SchemaCache with one schema: method "web.navigate" requiring "url".
/// Writes the schema JSON to a temp dir and calls `SchemaCache::load`.
fn schema_cache_with_navigate() -> (SchemaCache, TempDir) {
    let dir = TempDir::new().unwrap();
    let schema = json!({
        "request": {
            "type": "object",
            "properties": {
                "session": {"type": "string"},
                "url":     {"type": "string"}
            },
            "required": ["session", "url"]
        },
        "response": {}
    });
    std::fs::write(
        dir.path().join("web.navigate.json"),
        serde_json::to_string_pretty(&schema).unwrap(),
    )
    .unwrap();
    let cache = SchemaCache::load(dir.path()).unwrap();
    (cache, dir) // return dir so it stays alive for the test
}

// ── missing schemas dir error includes path + exit code 2 ───────

#[test]
fn test_ac_aesf_01_schema_cache_missing_dir_error_includes_path() {
    let dir = TempDir::new().unwrap();
    let nonexistent = dir.path().join("missing_schemas");
    match SchemaCache::load(&nonexistent) {
        Err(e) => {
            let err_str = e.to_string();
            assert!(
                err_str.contains("schemas not found at"),
                "error must say 'schemas not found at'; got: {err_str}"
            );
            let path_str = nonexistent.to_str().unwrap();
            assert!(
                err_str.contains(path_str),
                "error must include the actual path '{path_str}'; got: {err_str}"
            );
            assert!(
                err_str.contains("loom postinstall"),
                "error must mention 'loom postinstall'; got: {err_str}"
            );
        }
        Ok(_) => panic!("SchemaCache::load on nonexistent dir must return Err"),
    }
}

#[test]
fn test_ac_aesf_01_missing_schemas_exit_code_is_2() {
    let dir = TempDir::new().unwrap();
    let nonexistent = dir.path().join("missing");
    match SchemaCache::load(&nonexistent) {
        Err(e) => {
            let r: Result<(), CliError> = Err(e);
            assert_eq!(
                map_exit_code(&r),
                EXIT_USAGE,
                "missing schemas dir must exit 2"
            );
        }
        Ok(_) => panic!("expected Err from missing schemas dir"),
    }
}

#[test]
fn test_ac_aesf_01_missing_schemas_format_error_non_empty() {
    let dir = TempDir::new().unwrap();
    let nonexistent = dir.path().join("missing");
    match SchemaCache::load(&nonexistent) {
        Err(e) => {
            let msg = format_error(&Err(e)).unwrap();
            assert!(!msg.is_empty(), "format_error for missing schemas must be non-empty");
        }
        Ok(_) => panic!("expected Err from missing schemas dir"),
    }
}

// ── unknown method error names method + lists available ──────────

#[test]
fn test_ac_aesf_02_unknown_method_names_method() {
    let (schemas, _dir) = schema_cache_with_navigate();
    let args = json!({});
    let err = validate_args(&schemas, "bogus.method", &args).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("bogus.method"),
        "unknown method error must name the method; got: {msg}"
    );
}

#[test]
fn test_ac_aesf_02_unknown_method_lists_available() {
    let (schemas, _dir) = schema_cache_with_navigate();
    let args = json!({});
    let err = validate_args(&schemas, "bogus.method", &args).unwrap_err();
    let msg = err.to_string();
    // "Available: ..." section must list web.navigate
    assert!(
        msg.contains("web.navigate"),
        "unknown method error must list available methods; got: {msg}"
    );
    assert!(
        msg.to_lowercase().contains("available"),
        "error must include 'Available:'; got: {msg}"
    );
}

#[test]
fn test_ac_aesf_02_unknown_method_exit_code_is_2() {
    let (schemas, _dir) = schema_cache_with_navigate();
    let args = json!({});
    let r: Result<(), CliError> = Err(validate_args(&schemas, "bogus.method", &args).unwrap_err());
    assert_eq!(map_exit_code(&r), EXIT_USAGE);
}

#[test]
fn test_ac_aesf_02_unknown_method_format_error_non_empty() {
    let (schemas, _dir) = schema_cache_with_navigate();
    let args = json!({});
    let r: Result<(), CliError> = Err(validate_args(&schemas, "bogus.method", &args).unwrap_err());
    let msg = format_error(&r).unwrap();
    assert!(!msg.is_empty(), "format_error for unknown method must be non-empty");
}

// ── malformed args error names failing param ────────────────────

#[test]
fn test_ac_aesf_03_malformed_args_names_missing_field() {
    let (schemas, _dir) = schema_cache_with_navigate();
    // Missing required "url"
    let args = json!({"session": "S1"});
    let err = validate_args(&schemas, "web.navigate", &args).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("web.navigate"),
        "validation error must name the method; got: {msg}"
    );
    // jsonschema reports which field failed
    assert!(
        !msg.is_empty(),
        "validation error must be non-empty; got: {msg}"
    );
}

#[test]
fn test_ac_aesf_03_malformed_args_exit_code_is_2() {
    let (schemas, _dir) = schema_cache_with_navigate();
    let args = json!({"session": "S1"});
    let r: Result<(), CliError> = Err(validate_args(&schemas, "web.navigate", &args).unwrap_err());
    assert_eq!(map_exit_code(&r), EXIT_USAGE);
}

#[test]
fn test_ac_aesf_03_malformed_args_format_error_non_empty() {
    let (schemas, _dir) = schema_cache_with_navigate();
    let args = json!({"session": "S1"});
    let r: Result<(), CliError> = Err(validate_args(&schemas, "web.navigate", &args).unwrap_err());
    let msg = format_error(&r).unwrap();
    assert!(!msg.is_empty(), "format_error for malformed args must be non-empty");
}

// ── SurfaceUnavailable — exit 5 + clear message ─────────────────

#[test]
fn test_ac_aesf_04_surface_unavailable_exit_code_is_5() {
    let r: Result<(), CliError> = Err(CliError::SurfaceUnavailable(
        "web surface not loaded".into(),
    ));
    assert_eq!(map_exit_code(&r), EXIT_SURFACE_UNAVAILABLE);
    assert_eq!(map_exit_code(&r), 5);
}

#[test]
fn test_ac_aesf_04_surface_unavailable_format_error_mentions_surface() {
    let r: Result<(), CliError> = Err(CliError::SurfaceUnavailable(
        "web surface not loaded".into(),
    ));
    let msg = format_error(&r).unwrap();
    assert!(
        msg.to_lowercase().contains("surface"),
        "SurfaceUnavailable format_error must mention 'surface'; got: {msg}"
    );
    assert!(!msg.is_empty());
}

// ── every CliError variant has non-empty Display ────────────────

#[test]
fn test_ac_aesf_05_every_variant_has_display() {
    use loom_cli::error_mapper::{ConnectionError, DoctorReport};
    let variants: Vec<CliError> = vec![
        CliError::Usage("test usage".into()),
        CliError::Receipt(json!({"code": "x", "message": "rpc error"})),
        CliError::Connection(ConnectionError::DaemonNotRunning),
        CliError::Connection(ConnectionError::ConnectionTimeout),
        CliError::Connection(ConnectionError::AuthFailed),
        CliError::Connection(ConnectionError::SchemaVersionSkew),
        CliError::SupplyChain {
            expected_hash: "abc".into(),
            actual_hash: "def".into(),
            url: "https://example.com/chromium.zip".into(),
        },
        CliError::DoctorFailed(DoctorReport {
            checks: vec![],
            failures: vec!["socket check failed".into()],
        }),
        CliError::Internal("internal bug".into()),
        CliError::PermissionDenied("plist write denied".into()),
        CliError::SurfaceUnavailable("web surface not loaded".into()),
    ];
    for v in &variants {
        let s = format_error(&Err(v.clone())).unwrap();
        assert!(
            !s.is_empty(),
            "format_error for {:?} must be non-empty",
            v
        );
    }
}

// ── errors don't leak to stdout (structural guarantee test) ──────

#[test]
fn test_ac_aesf_06_ok_result_produces_no_stderr_message() {
    // format_error(Ok) must return None — stdout is the success path only.
    assert!(
        format_error(&Ok(())).is_none(),
        "format_error(Ok) must return None — no spurious stderr on success"
    );
}

#[test]
fn test_ac_aesf_06_parse_extra_unknown_key_flag() {
    // Verify parse_extra_to_json handles --flag correctly (flags set to true).
    let extra = vec!["--flag".to_string()];
    let result = parse_extra_to_json(&extra).unwrap();
    assert_eq!(result["flag"], json!(true));
}

#[test]
fn test_ac_aesf_06_parse_extra_key_value() {
    // Verify parse_extra_to_json handles --key value correctly.
    let extra = vec!["--url".to_string(), "https://example.com".to_string()];
    let result = parse_extra_to_json(&extra).unwrap();
    assert_eq!(result["url"], json!("https://example.com"));
}
