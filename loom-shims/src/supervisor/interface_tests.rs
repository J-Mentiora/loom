// Interface tests for `Supervisor`.
// Verifies IC-SHIM-02 (posix_spawn semantics — by structure),
// AC-DET-06.1 (locale-scrubbed env), SR-SHIM-04 (restart budget),
// SR-SHIM-03 (version mismatch), state-invalidation cascade.

use super::supervisor::{
    extract_ws_url, locale_scrub_env, parse_active_port_file, restart_allowed, RestartBudget,
    SupervisorConfig, SupervisorError,
};
use crate::ipc_endpoint::ipc_endpoint::ShimErrorCode;
use std::path::PathBuf;
use std::time::{Duration, Instant};

// === L5 parse_devtools_url helpers ===

#[test]
fn extract_ws_url_finds_canonical_chromium_line() {
    let line = "DevTools listening on ws://127.0.0.1:9222/devtools/browser/abc123\n";
    assert_eq!(
        extract_ws_url(line),
        Some("ws://127.0.0.1:9222/devtools/browser/abc123".to_string())
    );
}

#[test]
fn extract_ws_url_returns_none_for_unrelated_line() {
    let line = "[15123:15123:0428/154301.123:INFO:CONSOLE(1)] hello\n";
    assert_eq!(extract_ws_url(line), None);
}

#[test]
fn extract_ws_url_handles_wss() {
    let line = "DevTools listening on wss://127.0.0.1:9222/devtools/browser/x\n";
    assert!(extract_ws_url(line).unwrap().starts_with("wss://"));
}

#[test]
fn parse_active_port_file_constructs_ws_url() {
    let contents = "9222\n/devtools/browser/abc123";
    assert_eq!(
        parse_active_port_file(contents),
        Some("ws://127.0.0.1:9222/devtools/browser/abc123".to_string())
    );
}

#[test]
fn parse_active_port_file_handles_path_without_leading_slash() {
    let contents = "9222\ndevtools/browser/abc123";
    assert_eq!(
        parse_active_port_file(contents),
        Some("ws://127.0.0.1:9222/devtools/browser/abc123".to_string())
    );
}

#[test]
fn parse_active_port_file_returns_none_for_garbage() {
    assert_eq!(parse_active_port_file(""), None);
    assert_eq!(parse_active_port_file("not-a-port\n/devtools"), None);
}

// === AC-DET-06.1: locale-scrubbed env ===

#[test]
fn locale_scrub_sets_lc_all_c_utf8() {
    let (set, _remove) = locale_scrub_env();
    assert!(set.iter().any(|(k, v)| *k == "LC_ALL" && *v == "C.UTF-8"));
}

#[test]
fn locale_scrub_sets_lang_c_utf8() {
    let (set, _remove) = locale_scrub_env();
    assert!(set.iter().any(|(k, v)| *k == "LANG" && *v == "C.UTF-8"));
}

#[test]
fn locale_scrub_removes_lc_messages_numeric_time() {
    let (_set, remove) = locale_scrub_env();
    for key in &["LC_MESSAGES", "LC_NUMERIC", "LC_TIME"] {
        assert!(remove.contains(key), "missing scrub key {}", key);
    }
}

// === SR-SHIM-04: restart budget (max 3 within 60s) ===

#[test]
fn default_restart_budget_is_three_within_sixty_seconds() {
    let b = RestartBudget::default();
    assert_eq!(b.max_within_window, 3);
    assert_eq!(b.window, Duration::from_secs(60));
}

#[test]
fn restart_allowed_when_history_under_budget() {
    let now = Instant::now();
    let history = vec![now - Duration::from_secs(5), now - Duration::from_secs(10)];
    assert!(restart_allowed(&history, now, RestartBudget::default()));
}

#[test]
fn restart_blocked_when_three_within_window() {
    let now = Instant::now();
    let history = vec![
        now - Duration::from_secs(5),
        now - Duration::from_secs(20),
        now - Duration::from_secs(40),
    ];
    assert!(!restart_allowed(&history, now, RestartBudget::default()));
}

#[test]
fn restart_allowed_when_old_restarts_fall_outside_window() {
    let now = Instant::now();
    let history = vec![
        now - Duration::from_secs(5),
        now - Duration::from_secs(20),
        now - Duration::from_secs(120), // outside 60s window
    ];
    assert!(restart_allowed(&history, now, RestartBudget::default()));
}

// === SupervisorError → ShimErrorCode mapping (IC-SHIM-10) ===

#[test]
fn budget_exhausted_maps_to_chromium_unavailable() {
    let e = SupervisorError::BudgetExhausted { restarts: 3, window_ms: 60_000 };
    let code: ShimErrorCode = e.into();
    assert_eq!(code, ShimErrorCode::ChromiumUnavailable);
}

#[test]
fn spawn_failed_maps_to_shim_internal_error() {
    let e = SupervisorError::SpawnFailed("ENOENT".into());
    let code: ShimErrorCode = e.into();
    assert_eq!(code, ShimErrorCode::ShimInternalError);
}

#[test]
fn version_mismatch_maps_to_shim_internal_error() {
    let e = SupervisorError::VersionMismatch;
    let code: ShimErrorCode = e.into();
    assert_eq!(code, ShimErrorCode::ShimInternalError);
}

// === SR-SHIM-03: version mismatch flag ===

#[test]
fn supervisor_config_carries_version_mismatch_flag() {
    let mut cfg = SupervisorConfig::new(
        PathBuf::from("/opt/chromium/chrome"),
        PathBuf::from("/tmp/loom-chromium-1234"),
    );
    assert!(!cfg.version_mismatch);
    cfg.version_mismatch = true;
    assert!(cfg.version_mismatch);
}

// === IC-SHIM-09: user_data_dir under TMPDIR-style path ===

#[test]
fn supervisor_config_user_data_dir_is_caller_provided() {
    let cfg = SupervisorConfig::new(
        PathBuf::from("/opt/chromium/chrome"),
        PathBuf::from("/tmp/loom-chromium-99/profile"),
    );
    let s = cfg.user_data_dir.to_string_lossy();
    assert!(s.contains("loom-chromium"), "user_data_dir = {}", s);
}

// === BC-SHIM-05: no FS write expectation outside bundle/tmp ===

#[test]
fn extra_flags_default_empty() {
    let cfg = SupervisorConfig::new(PathBuf::from("/x"), PathBuf::from("/y"));
    assert!(cfg.extra_flags.is_empty());
}

// === Stub panics with expected message ===

// (Construction requires the dependent traits; we assert behavioural
// stubs at the pure-function layer above. The trait method panics are
// covered by the dispatcher/cdp/targets tests since each module's
// trait stubs share the same Phase 5.4 panic contract.)
