//! Integration tests for `cli-exit-code-receipt-error` feature.
//!
//! Covers AC-CLIEXIT-01 through AC-CLIEXIT-06: every error class maps
//! to the documented exit code, and `receipt_to_result` honours the
//! `status` field on receipt JSON.

use loom_cli::action_commands::validate_args;
use loom_cli::error_mapper::{
    map_exit_code, receipt_to_result, CliError, ConnectionError, EXIT_CONFIG, EXIT_DIFFERS,
    EXIT_OK, EXIT_PROTOCOL, EXIT_RECEIPT_ERROR, EXIT_SURFACE_UNAVAILABLE, EXIT_USAGE,
};
use loom_cli::schema_cache::SchemaCache;
use serde_json::json;
use tempfile::TempDir;

// ── helpers ─────────────────────────────────────────────────────────────────

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
    (cache, dir)
}

// ── AC-CLIEXIT-01: action error receipt → exit 1 ────────────────────────────

#[test]
fn test_ac_cliexit_01_action_receipt_error_maps_exit_1() {
    // Receipt has status="error" (the dogfood bug shape).
    let receipt = json!({
        "status": "error",
        "code": "selector_not_found",
        "message": "no element matching selector 'unknown'"
    });
    let r: Result<(), CliError> = Err(CliError::Receipt(receipt));
    assert_eq!(map_exit_code(&r), EXIT_RECEIPT_ERROR);
    assert_eq!(map_exit_code(&r), 1);
}

#[test]
fn test_ac_cliexit_01_receipt_to_result_status_error() {
    // The helper converts a status="error" receipt into Err(Receipt(v)).
    let receipt = json!({
        "status": "error",
        "code": "selector_not_found",
        "message": "x"
    });
    let r = receipt_to_result(receipt.clone());
    match r {
        Err(CliError::Receipt(v)) => assert_eq!(v, receipt),
        other => panic!("expected Err(Receipt), got {other:?}"),
    }
}

#[test]
fn test_ac_cliexit_01_receipt_to_result_status_ok_passes_through() {
    // status="ok" must NOT raise — this is the success path.
    let receipt = json!({"status": "ok", "result": {"x": 1}});
    let r = receipt_to_result(receipt.clone());
    assert_eq!(r.unwrap(), receipt);
}

#[test]
fn test_ac_cliexit_01_receipt_to_result_no_status_passes_through() {
    // Schema-shaped responses without a top-level status field
    // (e.g. session.create's session_id payload) are NOT errors.
    let resp = json!({"session_id": "S1", "started_at": "..."});
    let r = receipt_to_result(resp.clone());
    assert_eq!(r.unwrap(), resp);
}

// ── AC-CLIEXIT-02: URL allowlist denial → exit 1 (regression) ───────────────

#[test]
fn test_ac_cliexit_02_allowlist_denial_maps_exit_1() {
    // url_allowlist::check_url_scheme returns CliError::Receipt for denials.
    let denial_receipt = json!({
        "status": "error",
        "code": "url_scheme_disallowed",
        "message": "scheme 'file' is not in the allowlist"
    });
    let r: Result<(), CliError> = Err(CliError::Receipt(denial_receipt));
    assert_eq!(map_exit_code(&r), EXIT_RECEIPT_ERROR);
}

// ── AC-CLIEXIT-03: tampered session validation → exit 1 ─────────────────────

#[test]
fn test_ac_cliexit_03_validation_failure_maps_exit_1() {
    // session_commands::validate constructs a synthetic Receipt on FAIL.
    let synthetic = json!({
        "status": "error",
        "code": "session-validation-failed",
        "message": "session validation failed",
        "details": {"reasons": ["envelope MAC mismatch"]}
    });
    let r: Result<(), CliError> = Err(CliError::Receipt(synthetic));
    assert_eq!(map_exit_code(&r), EXIT_RECEIPT_ERROR);
    assert_eq!(map_exit_code(&r), 1);
}

#[test]
fn test_ac_cliexit_03_validate_synthetic_receipt_shape() {
    // The synthetic receipt must carry status=error AND a code, so
    // CliError::Receipt's Display produces an actionable line.
    let synthetic = json!({
        "status": "error",
        "code": "session-validation-failed",
        "message": "session validation failed",
        "details": {"reasons": ["envelope MAC mismatch"]}
    });
    let err = CliError::Receipt(synthetic);
    let msg = err.to_string();
    assert!(msg.contains("session-validation-failed"), "got: {msg}");
    assert!(msg.contains("session validation failed"), "got: {msg}");
}

// ── AC-CLIEXIT-04: closing already-closed session → exit 1 ──────────────────

#[test]
fn test_ac_cliexit_04_close_already_closed_maps_exit_1() {
    // The daemon may return a result-body receipt (status=error) for
    // session.close on a closed session. The helper raises it to exit 1.
    let resp = json!({
        "status": "error",
        "code": "session-already-closed",
        "message": "session S1 is already closed"
    });
    let r = receipt_to_result(resp).map(|_| ());
    assert_eq!(map_exit_code(&r), EXIT_RECEIPT_ERROR);
}

// ── AC-CLIEXIT-05: unknown action method → exit 2 (regression) ──────────────

#[test]
fn test_ac_cliexit_05_unknown_method_maps_exit_2() {
    let (schemas, _dir) = schema_cache_with_navigate();
    let args = json!({});
    let r: Result<(), CliError> =
        Err(validate_args(&schemas, "bogus.method", &args).unwrap_err());
    assert_eq!(map_exit_code(&r), EXIT_USAGE);
    assert_eq!(map_exit_code(&r), 2);
}

// ── AC-CLIEXIT-06: full exit-code table ─────────────────────────────────────

#[test]
fn test_ac_cliexit_06_table_exit_0_success() {
    let r: Result<(), CliError> = Ok(());
    assert_eq!(map_exit_code(&r), EXIT_OK);
    assert_eq!(map_exit_code(&r), 0);
}

#[test]
fn test_ac_cliexit_06_table_exit_0_receipt_status_ok() {
    // A receipt with status=ok wrapped in CliError::Receipt is treated as success
    // (existing behaviour preserved by map_exit_code).
    let r: Result<(), CliError> = Err(CliError::Receipt(json!({"status": "ok"})));
    assert_eq!(map_exit_code(&r), EXIT_OK);
}

#[test]
fn test_ac_cliexit_06_table_exit_1_action_error() {
    let r: Result<(), CliError> = Err(CliError::Receipt(json!({
        "status": "error",
        "code": "x",
        "message": "y"
    })));
    assert_eq!(map_exit_code(&r), 1);
}

#[test]
fn test_ac_cliexit_06_table_exit_1_connection_error() {
    let r: Result<(), CliError> = Err(CliError::Connection(ConnectionError::DaemonNotRunning));
    assert_eq!(map_exit_code(&r), 1);
}

#[test]
fn test_ac_cliexit_06_table_exit_2_usage() {
    let r: Result<(), CliError> = Err(CliError::Usage("bad arg".into()));
    assert_eq!(map_exit_code(&r), EXIT_USAGE);
    assert_eq!(map_exit_code(&r), 2);
}

#[test]
fn test_ac_cliexit_06_table_exit_3_config() {
    let r: Result<(), CliError> = Err(CliError::Config("config.toml: missing socket_path".into()));
    assert_eq!(map_exit_code(&r), EXIT_CONFIG);
    assert_eq!(map_exit_code(&r), 3);
}

#[test]
fn test_ac_cliexit_06_table_exit_4_protocol() {
    let r: Result<(), CliError> = Err(CliError::Protocol("malformed JSON-RPC envelope".into()));
    assert_eq!(map_exit_code(&r), EXIT_PROTOCOL);
    assert_eq!(map_exit_code(&r), 4);
}

#[test]
fn test_ac_cliexit_06_table_exit_5_surface_unavailable() {
    let r: Result<(), CliError> = Err(CliError::SurfaceUnavailable("web surface".into()));
    assert_eq!(map_exit_code(&r), EXIT_SURFACE_UNAVAILABLE);
    assert_eq!(map_exit_code(&r), 5);
}

#[test]
fn test_ac_cliexit_06_config_display_non_empty_and_mentions_class() {
    use loom_cli::error_mapper::format_error;
    let r: Result<(), CliError> = Err(CliError::Config("missing socket_path".into()));
    let msg = format_error(&r).unwrap();
    assert!(!msg.is_empty());
    assert!(
        msg.to_lowercase().contains("config"),
        "Config Display must mention class; got: {msg}"
    );
}

#[test]
fn test_ac_cliexit_06_protocol_display_non_empty_and_mentions_class() {
    use loom_cli::error_mapper::format_error;
    let r: Result<(), CliError> = Err(CliError::Protocol("malformed envelope".into()));
    let msg = format_error(&r).unwrap();
    assert!(!msg.is_empty());
    assert!(
        msg.to_lowercase().contains("protocol"),
        "Protocol Display must mention class; got: {msg}"
    );
}

// ── AC-CLIEXIT3-01: SessionsDiffer maps to dedicated exit 6 ─────────────────

#[test]
fn test_ac_cliexit3_01_table_exit_6_sessions_differ() {
    let r: Result<(), CliError> = Err(CliError::SessionsDiffer(
        "3 field diffs, action_count_delta=1".into(),
    ));
    assert_eq!(map_exit_code(&r), EXIT_DIFFERS);
    assert_eq!(map_exit_code(&r), 6);
}

#[test]
fn test_ac_cliexit3_01_sessions_differ_display_non_empty_and_mentions_class() {
    use loom_cli::error_mapper::format_error;
    let r: Result<(), CliError> = Err(CliError::SessionsDiffer(
        "2 field diffs, action_count_delta=0".into(),
    ));
    let msg = format_error(&r).unwrap();
    assert!(!msg.is_empty());
    assert!(
        msg.to_lowercase().contains("sessions differ"),
        "SessionsDiffer Display must mention class; got: {msg}"
    );
}

#[test]
fn test_ac_cliexit3_01_sessions_differ_distinct_from_internal() {
    // Regression guard: SessionsDiffer must NOT collapse back to exit 2 (Usage),
    // which is where it lived when emitted as CliError::Internal("sessions differ").
    let differ: Result<(), CliError> = Err(CliError::SessionsDiffer("x".into()));
    let internal: Result<(), CliError> = Err(CliError::Internal("x".into()));
    assert_ne!(
        map_exit_code(&differ),
        map_exit_code(&internal),
        "SessionsDiffer must map to a distinct exit code from Internal"
    );
    assert_eq!(map_exit_code(&differ), 6);
    assert_eq!(map_exit_code(&internal), EXIT_USAGE);
}
