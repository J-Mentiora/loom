//! Integration tests for `loom doctor`'s macOS Gatekeeper quarantine probe
//! (check 6 / AC2).
//!
//! The probe is RPC-free: it inspects a file path directly, so these tests
//! need no daemon fixture and carry no flake risk. The real logic is macOS-
//! only (`com.apple.quarantine` is an Apple concept); off macOS the check is a
//! no-op pass and only the cross-platform "absent binary passes" assertion
//! runs.

use loom_cli::cli_config::cli_config::compiled_defaults;
use loom_cli::cli_config::output_mode::OutputMode;
use loom_cli::doctor_runner::check_macos_quarantine_clear;
use loom_cli::output_formatter::emit;
use serde_json::json;

/// AC2 (human output): a failing check's detail — here the quarantine
/// remediation command — must reach the default pretty/curated view, not just
/// `--json`. Cross-platform: drives the renderer with a synthetic report.
#[test]
fn pretty_doctor_prints_failing_check_remediation() {
    let mut cfg = compiled_defaults();
    cfg.output_mode = OutputMode::PrettyCurated;
    cfg.stdout_color_enabled = false;

    let report = json!({
        "status": "error",
        "checks": [
            {"name": "socket_reachable", "status": "ok"},
            {
                "name": "macos_quarantine_clear",
                "status": "fail",
                "detail": "Error: /tmp/Chromium carries the macOS com.apple.quarantine \
                           attribute; clear it with: xattr -d com.apple.quarantine \
                           '/tmp/Chromium' — notarization is the committed follow-up."
            }
        ]
    });

    let out = emit("doctor", &report, &cfg, None).unwrap();
    assert!(
        out.contains("FAIL macos_quarantine_clear"),
        "failing check must be listed; got:\n{out}"
    );
    assert!(
        out.contains("xattr -d com.apple.quarantine '/tmp/Chromium'"),
        "failing check's remediation command must be printed in pretty mode; got:\n{out}"
    );
}

/// Passing checks carry no detail and must stay one-line (no stray blank/detail
/// lines) — guards the golden doctor fixture's shape.
#[test]
fn pretty_doctor_passing_check_has_no_detail_line() {
    let mut cfg = compiled_defaults();
    cfg.output_mode = OutputMode::PrettyCurated;
    cfg.stdout_color_enabled = false;

    let report = json!({
        "status": "ok",
        "checks": [{"name": "macos_quarantine_clear", "status": "ok"}]
    });
    let out = emit("doctor", &report, &cfg, None).unwrap();
    assert!(out.contains("OK macos_quarantine_clear"));
    assert!(
        !out.contains("notarization") && !out.contains("Error:"),
        "passing check must not emit a detail line; got:\n{out}"
    );
}

/// A missing binary is check 4's concern (presence + sha), not the quarantine
/// probe's — the probe must pass rather than double-report when there is
/// nothing to inspect. Runs on every platform.
#[tokio::test]
async fn absent_binary_passes() {
    let missing = std::path::Path::new("/definitely/not/here/Chromium");
    assert!(
        check_macos_quarantine_clear(missing).await.is_ok(),
        "absent binary must pass the quarantine probe (check 4 owns presence)"
    );
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use loom_cli::CliError;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;
    use tempfile::TempDir;

    fn set_quarantine(path: &std::path::Path) {
        let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
        let attr = CString::new("com.apple.quarantine").unwrap();
        // A representative quarantine value; the probe only checks presence,
        // so the exact contents are irrelevant.
        let value = b"0081;00000000;loom-test;";
        let ret = unsafe {
            libc::setxattr(
                c_path.as_ptr(),
                attr.as_ptr(),
                value.as_ptr() as *const libc::c_void,
                value.len(),
                0,
                0,
            )
        };
        assert_eq!(
            ret,
            0,
            "setxattr failed: {}",
            std::io::Error::last_os_error()
        );
    }

    fn remove_quarantine(path: &std::path::Path) {
        let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
        let attr = CString::new("com.apple.quarantine").unwrap();
        let ret = unsafe { libc::removexattr(c_path.as_ptr(), attr.as_ptr(), 0) };
        assert_eq!(
            ret,
            0,
            "removexattr failed: {}",
            std::io::Error::last_os_error()
        );
    }

    #[tokio::test]
    async fn clean_binary_passes() {
        let dir = TempDir::new().unwrap();
        let bin = dir.path().join("Chromium");
        std::fs::write(&bin, b"fake-chromium").unwrap();
        assert!(
            check_macos_quarantine_clear(&bin).await.is_ok(),
            "a binary without com.apple.quarantine must pass"
        );
    }

    #[tokio::test]
    async fn quarantined_binary_fails_with_remediation() {
        let dir = TempDir::new().unwrap();
        let bin = dir.path().join("Chromium");
        std::fs::write(&bin, b"fake-chromium").unwrap();
        set_quarantine(&bin);

        let err = check_macos_quarantine_clear(&bin)
            .await
            .expect_err("a quarantined binary must fail the probe");

        // The detail flows into the DoctorReport (and the pretty renderer) via
        // CliError's Display, so assert on Display — that is what a user sees.
        let msg = err.to_string();
        let path_str = bin.display().to_string();

        assert!(
            matches!(err, CliError::Internal(_)),
            "expected Internal; got {err:?}"
        );
        // AC2: names the file.
        assert!(
            msg.contains(&path_str),
            "message must name the file; got: {msg}"
        );
        // AC2: prints the exact removal command, path single-quoted.
        assert!(
            msg.contains(&format!("xattr -d com.apple.quarantine '{path_str}'")),
            "message must print the exact single-quoted xattr removal command; got: {msg}"
        );
        // AC2: one-line "why".
        assert!(
            msg.contains("Gatekeeper"),
            "message must briefly explain why the attribute exists; got: {msg}"
        );
        // AC2: notarization follow-up note.
        assert!(
            msg.contains("notarization"),
            "message must note the notarization follow-up; got: {msg}"
        );
    }

    #[tokio::test]
    async fn clearing_quarantine_makes_it_pass_again() {
        let dir = TempDir::new().unwrap();
        let bin = dir.path().join("Chromium");
        std::fs::write(&bin, b"fake-chromium").unwrap();

        set_quarantine(&bin);
        assert!(
            check_macos_quarantine_clear(&bin).await.is_err(),
            "quarantined binary should fail before the attribute is cleared"
        );

        remove_quarantine(&bin); // mirrors the remediation command
        assert!(
            check_macos_quarantine_clear(&bin).await.is_ok(),
            "after clearing the attribute the probe must pass"
        );
    }

    #[tokio::test]
    async fn path_with_single_quote_is_shell_escaped() {
        // A single quote in the path must be escaped as '\'' so the printed
        // command cannot break out of the quoting (copy-paste injection guard).
        let dir = TempDir::new().unwrap();
        let bin = dir.path().join("Chro'mium");
        std::fs::write(&bin, b"fake").unwrap();
        set_quarantine(&bin);

        let msg = check_macos_quarantine_clear(&bin)
            .await
            .unwrap_err()
            .to_string();
        let path_str = bin.display().to_string();
        let escaped = format!("'{}'", path_str.replace('\'', "'\\''"));
        assert!(
            msg.contains(&format!("xattr -d com.apple.quarantine {escaped}")),
            "single quote in path must be shell-escaped; got: {msg}"
        );
    }
}
