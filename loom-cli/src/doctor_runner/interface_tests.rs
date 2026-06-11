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
