// Interface tests for `DoctorRunner`. Verifies the exactly-8
// check enumeration in stable order.

use super::doctor_runner::{DoctorArgs, DoctorPaths, CHECK_NAMES};

#[test]
fn check_names_count_is_exactly_nine() {
    assert_eq!(
        CHECK_NAMES.len(),
        9,
        "expected exactly 9 checks; got {}",
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
            "aot_artifacts_current",
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
// The individual `check_*` functions below MUST exist; their absence
// breaks compilation of `interface_tests`, which is the structural
// enforcement.
#[test]
fn individual_check_functions_compile() {
    // Reference each function by signature.
    use super::doctor_runner::{
        check_aot_artifacts, check_aot_artifacts_current, check_browser_smoke, check_chromium,
        check_daemon_responsive, check_keychain_acl, check_macos_quarantine_clear,
        check_session_health, check_socket_reachable,
    };
    let _ = check_socket_reachable;
    let _ = check_daemon_responsive;
    let _ = check_aot_artifacts;
    let _ = check_aot_artifacts_current;
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

// === aot_artifacts_current — stale-surface guard (RPC-free) ===
//
// These need loom-host (for the embedded SHA + engine-free compat hash), so they
// are gated on the `postinstall` feature (default-on; the non-postinstall build
// reports this check `skipped` under `--daemon-only`).

#[cfg(feature = "postinstall")]
#[tokio::test]
async fn aot_current_passes_when_artifact_absent() {
    use super::doctor_runner::check_aot_artifacts_current;
    use tempfile::TempDir;
    // No cwasm/sidecar at all → check 3 ("present") owns that; current passes.
    let dir = TempDir::new().unwrap();
    assert!(check_aot_artifacts_current(dir.path()).await.is_ok());
}

#[cfg(feature = "postinstall")]
#[tokio::test]
async fn aot_current_passes_when_stamp_matches_this_binary() {
    use super::doctor_runner::check_aot_artifacts_current;
    use loom_host::surface_stamp::{embedded_surface_web_sha256, format_surface_sidecar};
    use loom_host::wasm_runtime::{precompile_compatibility_hash_for, WasmRuntimeConfig};
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("loom_surface_web.cwasm"), b"x").unwrap();
    // Stamp that exactly matches THIS binary: embedded source SHA + default-config
    // compat hash (what `loom postinstall` writes).
    let compat = precompile_compatibility_hash_for(&WasmRuntimeConfig::default().opt_level);
    std::fs::write(
        dir.path().join("loom_surface_web.sha256"),
        format_surface_sidecar(embedded_surface_web_sha256(), &compat),
    )
    .unwrap();
    assert!(
        check_aot_artifacts_current(dir.path()).await.is_ok(),
        "a current composite stamp must pass"
    );
}

#[cfg(feature = "postinstall")]
#[tokio::test]
async fn aot_current_fails_on_stale_engine_compat() {
    use super::doctor_runner::check_aot_artifacts_current;
    use loom_host::surface_stamp::{embedded_surface_web_sha256, format_surface_sidecar};
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("loom_surface_web.cwasm"), b"x").unwrap();
    // Correct source SHA (so the source strand is satisfied / skipped if empty),
    // but a compat line that cannot match the live engine.
    std::fs::write(
        dir.path().join("loom_surface_web.sha256"),
        format_surface_sidecar(embedded_surface_web_sha256(), "wh-stale-engine-wt0.0.0"),
    )
    .unwrap();
    let err = check_aot_artifacts_current(dir.path())
        .await
        .expect_err("a stale engine-compat stamp must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("stale surface artifact") && msg.contains("loom postinstall"),
        "want typed stale-surface remediation, got: {msg}"
    );
}

#[cfg(feature = "postinstall")]
#[tokio::test]
async fn aot_current_passes_on_legacy_single_line_sidecar() {
    use super::doctor_runner::check_aot_artifacts_current;
    use loom_host::surface_stamp::embedded_surface_web_sha256;
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("loom_surface_web.cwasm"), b"x").unwrap();
    // Legacy single-line sidecar (source SHA only, no compat line). The daemon
    // would still boot a compatible legacy artifact, so doctor must not false-red.
    std::fs::write(
        dir.path().join("loom_surface_web.sha256"),
        embedded_surface_web_sha256(),
    )
    .unwrap();
    assert!(
        check_aot_artifacts_current(dir.path()).await.is_ok(),
        "a legacy single-line sidecar must not be flagged stale"
    );
}
