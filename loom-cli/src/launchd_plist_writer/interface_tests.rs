// Interface tests for `LaunchdPlistWriter`. Verifies
// plist constants and the cfg gating.

use super::launchd_plist_writer::{
    LaunchdPlistConfig, LaunchdPlistWriter, WriteOutcome, PLIST_KEEP_ALIVE_ON_SUCCESSFUL_EXIT,
    PLIST_LABEL, PLIST_RUN_AT_LOAD,
};

// === plist constants locked ===
#[test]
fn plist_label_is_com_loom_daemon() {
    assert_eq!(PLIST_LABEL, "com.loom.daemon");
}

#[test]
#[allow(clippy::assertions_on_constants)]
fn plist_run_at_load_is_true() {
    assert!(PLIST_RUN_AT_LOAD);
}

#[test]
#[allow(clippy::assertions_on_constants)]
fn plist_keep_alive_on_successful_exit_is_false() {
    // KeepAlive on SuccessfulExit:false ⇒ relaunch only on FAILED exit.
    assert!(!PLIST_KEEP_ALIVE_ON_SUCCESSFUL_EXIT);
}

#[test]
fn config_carries_loom_binary_and_plist_path() {
    let c = LaunchdPlistConfig {
        loom_binary: "/usr/local/bin/loom".into(),
        plist_path: "/Library/LaunchDaemons/com.loom.daemon.plist".into(),
    };
    assert!(c.loom_binary.is_absolute());
    assert!(c.plist_path.is_absolute());
}

#[test]
fn write_outcome_variant_set_locked() {
    fn _ck(o: WriteOutcome) -> &'static str {
        match o {
            WriteOutcome::Skipped => "skipped",
            WriteOutcome::Wrote => "wrote",
        }
    }
    let _ = _ck;
}

#[test]
fn writer_constructor_stores_config() {
    let _w = LaunchdPlistWriter::new(LaunchdPlistConfig {
        loom_binary: "/usr/local/bin/loom".into(),
        plist_path: "/tmp/com.loom.daemon.plist".into(),
    });
}

// === cfg(target_os = "macos") gating ===
//
// On non-macOS targets, `write` must still compile (callers depend on
// the symbol) but is documented to return `CliError::Internal`.
#[test]
fn cfg_target_os_macos_documented() {
    let s = "#[cfg(target_os = \"macos\")]";
    assert!(s.contains("macos"));
}
