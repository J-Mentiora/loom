// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-cli/modules/ErrorMapper/interface_tests.rs` instead.
// Interface tests for `ErrorMapper`. Verifies IC-CLI-04 exit-code
// constants, BC-CLI-05 1:1 mapping signature, and the actionable
// connection-message catalog (steal from C-02).

use super::error_mapper::{
    connection_message, format_error, map_exit_code, CliError, ConnectionError, DoctorCheck,
    DoctorReport, EXIT_OK, EXIT_RECEIPT_ERROR, EXIT_SURFACE_UNAVAILABLE, EXIT_USAGE,
};

// === IC-CLI-04: exit codes 0/1/2/5 ===
#[test]
fn exit_code_constants_locked() {
    assert_eq!(EXIT_OK, 0);
    assert_eq!(EXIT_RECEIPT_ERROR, 1);
    assert_eq!(EXIT_USAGE, 2);
    assert_eq!(EXIT_SURFACE_UNAVAILABLE, 5);
}

#[test]
fn map_exit_code_signature() {
    fn _ck(r: &Result<(), CliError>) -> i32 {
        map_exit_code(r)
    }
    let _ = _ck;
}

// === Actionable connection messages (C-02 steal) ===
#[test]
fn daemon_not_running_message_names_loom_serve() {
    let m = connection_message(&ConnectionError::DaemonNotRunning);
    assert!(
        m.contains("loom serve"),
        "DaemonNotRunning message must direct user to `loom serve`; got: {m}"
    );
}

#[test]
fn timeout_message_names_loom_doctor() {
    let m = connection_message(&ConnectionError::ConnectionTimeout);
    assert!(
        m.contains("loom doctor"),
        "ConnectionTimeout message must direct user to `loom doctor`; got: {m}"
    );
}

#[test]
fn auth_failed_message_mentions_hello() {
    let m = connection_message(&ConnectionError::AuthFailed);
    assert!(m.to_lowercase().contains("hello"));
}

#[test]
fn schema_skew_message_mentions_reinstall() {
    let m = connection_message(&ConnectionError::SchemaVersionSkew);
    assert!(m.to_lowercase().contains("reinstall"));
}

// === CliError variant set locked ===
#[test]
fn cli_error_variant_set_locked() {
    fn _ck(e: CliError) -> &'static str {
        match e {
            CliError::Usage(_) => "usage",
            CliError::Receipt(_) => "receipt",
            CliError::Connection(_) => "connection",
            CliError::SupplyChain { .. } => "supply_chain",
            CliError::DoctorFailed(_) => "doctor_failed",
            CliError::Internal(_) => "internal",
            CliError::PermissionDenied(_) => "permission_denied",
            CliError::SurfaceUnavailable(_) => "surface_unavailable",
            CliError::Config(_) => "config",
            CliError::Protocol(_) => "protocol",
            CliError::SessionsDiffer(_) => "sessions_differ",
            CliError::BrowserNotFound(_) => "browser_not_found",
        }
    }
    let _ = _ck;
}

// === AC-AESF-04: SurfaceUnavailable exits 5 ===
#[test]
fn surface_unavailable_maps_to_exit_5() {
    let r: Result<(), CliError> = Err(CliError::SurfaceUnavailable("web surface not loaded".into()));
    assert_eq!(map_exit_code(&r), EXIT_SURFACE_UNAVAILABLE);
    assert_eq!(map_exit_code(&r), 5);
}

// === AC-AESF-05: format_error never loses a message ===
#[test]
fn format_error_ok_returns_none() {
    assert!(format_error(&Ok(())).is_none());
}

#[test]
fn format_error_usage_returns_error_prefix() {
    let r: Result<(), CliError> = Err(CliError::Usage("unknown method: foo".into()));
    let msg = format_error(&r).unwrap();
    assert!(msg.contains("unknown method: foo"), "got: {msg}");
    assert!(!msg.is_empty());
}

#[test]
fn format_error_internal_returns_message() {
    let r: Result<(), CliError> = Err(CliError::Internal("schemas not found at /tmp/x — run loom postinstall".into()));
    let msg = format_error(&r).unwrap();
    assert!(msg.contains("schemas not found"), "got: {msg}");
}

#[test]
fn format_error_surface_unavailable_names_surface() {
    let r: Result<(), CliError> = Err(CliError::SurfaceUnavailable("web surface not loaded".into()));
    let msg = format_error(&r).unwrap();
    assert!(
        msg.to_lowercase().contains("surface"),
        "format_error(SurfaceUnavailable) must mention 'surface'; got: {msg}"
    );
    assert!(!msg.is_empty());
}

#[test]
fn format_error_connection_returns_actionable_message() {
    let r: Result<(), CliError> = Err(CliError::Connection(ConnectionError::DaemonNotRunning));
    let msg = format_error(&r).unwrap();
    assert!(
        msg.contains("loom serve"),
        "DaemonNotRunning format_error must mention 'loom serve'; got: {msg}"
    );
}

#[test]
fn format_error_receipt_error_returns_message() {
    let r: Result<(), CliError> = Err(CliError::Receipt(serde_json::json!({
        "code": "surface_unavailable",
        "message": "web surface not loaded"
    })));
    let msg = format_error(&r).unwrap();
    assert!(!msg.is_empty(), "Receipt error must produce non-empty format_error output");
    assert!(
        msg.contains("surface_unavailable"),
        "Receipt format_error must surface the wire `code`; got: {msg}"
    );
    assert!(
        msg.contains("web surface not loaded"),
        "Receipt format_error must surface the wire `message`; got: {msg}"
    );
}

#[test]
fn format_error_receipt_includes_data_when_present() {
    // AC-CLIERR-01: when daemon returns {code,message,data}, all three surface.
    let r: Result<(), CliError> = Err(CliError::Receipt(serde_json::json!({
        "code": "surface_trap",
        "message": "action dispatch failed",
        "data": {"trap_kind": "out_of_gas", "module": "web"}
    })));
    let msg = format_error(&r).unwrap();
    assert!(msg.contains("surface_trap"), "code must appear; got: {msg}");
    assert!(msg.contains("action dispatch failed"), "message must appear; got: {msg}");
    assert!(msg.contains("out_of_gas"), "data payload must appear; got: {msg}");
    assert!(msg.contains("module"), "data keys must appear; got: {msg}");
}

#[test]
fn format_error_receipt_omits_data_when_null() {
    // null data must not produce a trailing `data:` line.
    let r: Result<(), CliError> = Err(CliError::Receipt(serde_json::json!({
        "code": "schema_violation",
        "message": "bad request",
        "data": serde_json::Value::Null
    })));
    let msg = format_error(&r).unwrap();
    assert!(
        !msg.contains("data:"),
        "null data must not emit a `data:` continuation line; got: {msg}"
    );
}

#[test]
fn format_error_receipt_omits_data_when_absent() {
    let r: Result<(), CliError> = Err(CliError::Receipt(serde_json::json!({
        "code": "schema_violation",
        "message": "bad request"
    })));
    let msg = format_error(&r).unwrap();
    assert!(
        !msg.contains("data:"),
        "absent data must not emit a `data:` continuation line; got: {msg}"
    );
}

// AC-AESF-05: compile-time exhaustiveness — every CliError variant has Display
#[test]
fn all_variants_have_display_impl() {
    // Tests that Display is implemented by calling to_string() on each variant.
    // Compiler will fail this test file if Display is not implemented.
    let variants: Vec<CliError> = vec![
        CliError::Usage("test".into()),
        CliError::Receipt(serde_json::json!({"code": "x", "message": "y"})),
        CliError::Connection(ConnectionError::DaemonNotRunning),
        CliError::SupplyChain {
            expected_hash: "abc".into(),
            actual_hash: "def".into(),
            url: "https://example.com".into(),
        },
        CliError::DoctorFailed(DoctorReport { checks: vec![], failures: vec!["bad".into()] }),
        CliError::Internal("bug".into()),
        CliError::PermissionDenied("no root".into()),
        CliError::SurfaceUnavailable("web not loaded".into()),
    ];
    for v in variants {
        let s = v.to_string();
        assert!(!s.is_empty(), "Display for CliError variant must be non-empty");
    }
}

// === AC-CHPIN-08: PermissionDenied exits 0 (graceful degrade) ===
#[test]
fn permission_denied_maps_to_exit_ok() {
    let r: Result<(), CliError> = Err(CliError::PermissionDenied("test".into()));
    assert_eq!(map_exit_code(&r), EXIT_OK);
}

#[test]
fn doctor_report_carries_checks_and_failures() {
    let r = DoctorReport {
        checks: vec![DoctorCheck {
            name: "socket".into(),
            status: "ok".into(),
            detail: None,
        }],
        failures: vec![],
    };
    assert_eq!(r.checks.len(), 1);
}

// === BC-CLI-05: 1:1 mirror enforced by tools/lint-error-codes.py ===
//
// The lint script lives outside this crate; here we lock the public
// surface used by the script (the `CliError` variant set above).
#[test]
fn receipt_carries_serde_json_value_for_passthrough() {
    // SR-CLI-03 — receipt flows verbatim. Encoded by the variant
    // holding `serde_json::Value` (no field rewriting).
    let e = CliError::Receipt(serde_json::json!({
        "status": "error",
        "code": "schema_violation"
    }));
    let s = serde_json::to_string(&e).unwrap();
    assert!(s.contains("schema_violation"));
}
