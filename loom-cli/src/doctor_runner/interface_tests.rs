// Interface tests for `DoctorRunner`. Verifies the exactly-8
// check enumeration in stable order.

use super::doctor_runner::{DoctorArgs, DoctorPaths, CHECK_NAMES};

#[test]
fn check_names_count_is_exactly_eight() {
    assert_eq!(
        CHECK_NAMES.len(),
        8,
        "expected exactly 8 checks; got {}",
        CHECK_NAMES.len()
    );
}

#[test]
fn check_names_are_in_stable_order() {
    assert_eq!(
        CHECK_NAMES,
        &[
            "socket_reachable",
            "daemon_responsive",
            "aot_artifacts_present",
            "chromium_present_and_verified",
            "vault_keychain_accessible",
            "macos_quarantine_clear",
            "session_health",
            "browser_smoke",
        ]
    );
}

#[test]
fn doctor_args_default_is_full_probe() {
    let args = DoctorArgs::default();
    assert!(!args.daemon_only, "default must run the full 8-check probe");
}

#[test]
fn doctor_paths_carries_all_5_check_inputs() {
    let p = DoctorPaths {
        socket_path: "/tmp/loom.sock".into(),
        surfaces_dir: "/tmp/surfaces".into(),
        chromium_binary: "/tmp/Chromium".into(),
        chromium_expected_sha256: "abc".into(),
        keychain_label: "com.loom.vault.user".into(),
    };
    // every field is populated → every check has its inputs available.
    assert!(p.socket_path.is_absolute());
}

// === missing any check is a KILL ===
//
// The 7 individual `check_*` functions below MUST exist; their
// absence breaks compilation of `interface_tests`, which is the
// structural enforcement.
#[test]
fn seven_individual_check_functions_compile() {
    // Reference each function by signature.
    use super::doctor_runner::{
        check_aot_artifacts, check_browser_smoke, check_chromium, check_daemon_responsive,
        check_keychain_acl, check_macos_quarantine_clear, check_session_health,
        check_socket_reachable,
    };
    let _ = check_socket_reachable;
    let _ = check_daemon_responsive;
    let _ = check_aot_artifacts;
    let _ = check_chromium;
    let _ = check_keychain_acl;
    let _ = check_macos_quarantine_clear;
    let _ = check_session_health;
    let _ = check_browser_smoke;
}

// === browser_smoke at-capacity classification (typed-capacity-errors) ===
//
// A saturated-but-healthy daemon must surface as the warn-class
// `at_capacity` status, never a `fail` — only the daemon's TYPED
// `session_cap_exceeded` envelope qualifies.

#[test]
fn session_cap_detail_recognizes_typed_cap_envelope_with_counts() {
    use super::doctor_runner::session_cap_detail;
    use crate::CliError;

    let e = CliError::Receipt(serde_json::json!({
        "code": "session_cap_exceeded",
        "message": "concurrent session cap reached (16/16)",
        "data": { "active": 16, "cap": 16,
                  "hint": "close sessions or run `loom session reap`" },
    }));
    let detail = session_cap_detail(&e).expect("typed cap envelope must classify");
    assert!(
        detail.contains("active_sessions=16") && detail.contains("cap=16"),
        "detail must agree with session_health's counts: {detail}"
    );
    assert!(
        detail.contains("loom session reap"),
        "detail must carry the remediation: {detail}"
    );
}

#[test]
fn session_cap_detail_tolerates_missing_data_counts() {
    use super::doctor_runner::session_cap_detail;
    use crate::CliError;

    // Typed code but no structured data (defensive: older daemon shape).
    let e = CliError::Receipt(serde_json::json!({ "code": "session_cap_exceeded" }));
    let detail = session_cap_detail(&e).expect("code alone must still classify");
    assert!(detail.contains("at capacity"), "got: {detail}");
}

#[test]
fn session_cap_detail_rejects_other_errors() {
    use super::doctor_runner::session_cap_detail;
    use crate::CliError;

    // A genuine failure (the historic opaque shape) must stay a failure.
    let internal = CliError::Receipt(serde_json::json!({
        "code": "internal_error",
        "message": "session.create failed",
    }));
    assert!(session_cap_detail(&internal).is_none());

    // Non-receipt errors (connection faults etc.) must stay failures too.
    let conn = CliError::Connection(crate::error_mapper::ConnectionError::DaemonNotRunning);
    assert!(session_cap_detail(&conn).is_none());
}
