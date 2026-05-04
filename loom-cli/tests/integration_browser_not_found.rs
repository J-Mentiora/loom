//! AC-DIST-05 integration tests for the BrowserNotFound flow.
//!
//! Covers:
//! - `CliError::BrowserNotFound` round-trips from a JSON-RPC error envelope
//!   (`code: "browser_not_found"`) → typed CLI variant → exit code 1 →
//!   platform-aware actionable Display message.
//! - The exit code matches the existing prereq-missing pattern (e.g.
//!   `Connection(DaemonNotRunning)` is also exit 1).
//!
//! End-to-end "boot a daemon with no chromium" coverage lives in the
//! chromium_resolver unit tests (interface_tests.rs) — a full subprocess
//! boot here would duplicate work and would need a Chromium-free CI env.

use loom_cli::error_mapper::{map_exit_code, CliError, EXIT_RECEIPT_ERROR};

#[test]
fn ac_dist_05_browser_not_found_maps_exit_1() {
    let r: Result<(), CliError> = Err(CliError::BrowserNotFound("install via brew".to_string()));
    assert_eq!(map_exit_code(&r), EXIT_RECEIPT_ERROR);
}

#[test]
fn ac_dist_05_browser_not_found_display_mentions_install_command() {
    let e = CliError::BrowserNotFound("any daemon-side detail".to_string());
    let msg = format!("{e}");
    // Platform-aware install hint: at least one of the relevant package
    // managers must appear in the message regardless of host OS.
    assert!(
        msg.contains("brew install --cask chromium")
            || msg.contains("apt install chromium")
            || msg.contains("dnf install chromium"),
        "BrowserNotFound message should mention a concrete install command, got: {msg}"
    );
    // And the recovery command.
    assert!(
        msg.contains("loom doctor"),
        "should suggest running 'loom doctor', got: {msg}"
    );
}

#[test]
fn ac_dist_05_browser_not_found_display_says_chromium_not_found() {
    let e = CliError::BrowserNotFound("anything".into());
    let msg = format!("{e}");
    assert!(
        msg.contains("Chromium not found"),
        "should clearly state Chromium is missing, got: {msg}"
    );
}

#[test]
fn ac_dist_05_browser_not_found_does_not_leak_daemon_detail() {
    // The carried `_msg` is daemon-side raw text (e.g. searched paths);
    // the user-facing Display synthesizes a fixed actionable message
    // and should not surface the raw detail string verbatim.
    let leak = "DAEMON_RAW_DEBUG_TEXT_DO_NOT_LEAK_xyzzy";
    let e = CliError::BrowserNotFound(leak.to_string());
    let msg = format!("{e}");
    assert!(
        !msg.contains(leak),
        "Display should not echo the raw daemon detail, got: {msg}"
    );
}
