// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-cli/modules/DoctorRunner/interface_tests.rs` instead.
// Interface tests for `DoctorRunner`. Verifies IC-CLI-07's exactly-5
// check enumeration in stable order.

use super::doctor_runner::{DoctorArgs, DoctorPaths, CHECK_NAMES};

#[test]
fn check_names_count_is_exactly_five() {
    assert_eq!(
        CHECK_NAMES.len(),
        5,
        "IC-CLI-07 mandates exactly 5 checks; got {}",
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
        ]
    );
}

#[test]
fn doctor_args_is_empty() {
    let _ = DoctorArgs::default();
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

// === IC-CLI-07: missing any check is a KILL ===
//
// The 5 individual `check_*` functions below MUST exist; their
// absence breaks compilation of `interface_tests`, which is the
// structural enforcement of IC-CLI-07.
#[test]
fn five_individual_check_functions_compile() {
    // Reference each function by signature.
    use super::doctor_runner::{
        check_aot_artifacts, check_chromium, check_daemon_responsive, check_keychain_acl,
        check_socket_reachable,
    };
    let _ = check_socket_reachable;
    let _ = check_daemon_responsive;
    let _ = check_aot_artifacts;
    let _ = check_chromium;
    let _ = check_keychain_acl;
}
