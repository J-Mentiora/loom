//! End-to-end coverage for the configurable per-CDP-command navigate budget
//! (`LOOM_SHIM_CDP_TIMEOUT_MS`).
//!
//! Drives `ShimManager::send_navigate` against the real `loom-shim-chromium`
//! binary, which spawns the test-only `fake-chromium`. fake-chromium's
//! `http://fake.test/slow/<MS>` URL pattern stalls the `Page.navigate`
//! response by <MS> milliseconds — the binding CDP roundtrip — so the shim's
//! per-command navigate timeout can be exercised deterministically:
//!
//!   (a) budget RAISED above the delay  → navigate SUCCEEDS.
//!   (b) budget at/under the delay       → typed `Err(LoomErrorCode::ShimTimeout)`
//!                                          (wire `shim_timeout`), distinct from
//!                                          a broken page (`SurfaceTrap`).
//!
//! The knob is read once per process (a cached `OnceLock`), so each case spawns
//! its OWN shim child with its own `LOOM_SHIM_CDP_TIMEOUT_MS` — the per-process
//! env on `ShimConfig` sidesteps the process-global cache.
//!
//! Run:
//!   cargo build -p loom-shims --features fake-chromium-bin --bin fake-chromium
//!   cargo build -p loom-cli --bin loom-shim-chromium
//!   cargo test -p loom-host --test integration_navigate_timeout_budget -- --ignored
//!
//! Marked `#[ignore]` so a default `cargo test --workspace` doesn't force the
//! fake-chromium build.

#![cfg(unix)]

use loom_core::error::LoomErrorCode;
use loom_host::host_observability::HostObservability;
use loom_host::shim_manager::{ShimConfig, ShimId, ShimManager};
use std::time::Duration;

fn target_bin_dir() -> std::path::PathBuf {
    let test_exe = std::env::current_exe().expect("current_exe");
    let deps = test_exe.parent().expect("deps dir");
    deps.parent().expect("debug dir").to_path_buf()
}

fn shim_bin() -> String {
    target_bin_dir()
        .join("loom-shim-chromium")
        .to_string_lossy()
        .into_owned()
}

fn fake_chromium_bin() -> String {
    target_bin_dir()
        .join("fake-chromium")
        .to_string_lossy()
        .into_owned()
}

fn assert_binaries_built() {
    let fake_path = fake_chromium_bin();
    let shim_path = shim_bin();
    if !std::path::Path::new(&fake_path).exists() {
        panic!(
            "fake-chromium binary not built at {fake_path}; run `cargo build -p loom-shims --features fake-chromium-bin --bin fake-chromium` first"
        );
    }
    if !std::path::Path::new(&shim_path).exists() {
        panic!(
            "loom-shim-chromium binary not built at {shim_path}; run `cargo build -p loom-cli --bin loom-shim-chromium` first"
        );
    }
}

/// Register a shim whose child process sees `LOOM_SHIM_CDP_TIMEOUT_MS=<ms>`,
/// so its cached `navigate_budget()` resolves to that value.
fn make_manager(
    session_label: &str,
    cdp_timeout_ms: &str,
) -> (std::sync::Arc<ShimManager>, ShimId, tempfile::TempDir) {
    let user_data_dir = tempfile::tempdir().expect("tempdir");
    let obs = HostObservability::new(true);
    let mgr = ShimManager::new(obs);
    let id = ShimId(format!("chromium:{session_label}"));
    mgr.register(
        id.clone(),
        ShimConfig {
            binary_path: shim_bin().into(),
            args: vec![],
            env: vec![
                ("LOOM_SHIM_CHROMIUM_PATH".into(), fake_chromium_bin()),
                (
                    "LOOM_SHIM_USER_DATA_DIR".into(),
                    user_data_dir.path().display().to_string(),
                ),
                (
                    "LOOM_FAKE_CHROMIUM_USER_DATA_DIR".into(),
                    user_data_dir.path().display().to_string(),
                ),
                ("LOOM_SHIM_CDP_TIMEOUT_MS".into(), cdp_timeout_ms.into()),
            ],
            spawn_retry: 1,
            breaker_threshold: 3,
            breaker_open_ms: 5_000,
            send_timeout_ms: 30_000,
            recv_timeout_ms: 60_000,
        },
    );
    (mgr, id, user_data_dir)
}

async fn navigate(
    mgr: &std::sync::Arc<ShimManager>,
    id: ShimId,
    url: &str,
) -> Result<loom_shared::navigate_outcome::NavigateOutcome, loom_core::error::LoomError> {
    tokio::time::timeout(
        Duration::from_secs(45),
        mgr.send_navigate(loom_host::shim_manager::SendNavigateParams {
            id,
            action_id: "test-action".to_string(),
            session_id: 0,
            target_id: 0,
            url: url.to_string(),
            // Host-side outer budget — deliberately large so the SHIM's
            // per-command timeout (not this) is the one under test.
            budget_ms: 30_000,
            seed: loom_shared::types::Seed(0),
            epoch_ms: loom_shared::types::EpochMs(0),
            blocklist_enabled: true,
            until: "settled".to_string(),
            determinism_enabled: true,
            audio_enabled: false,
        }),
    )
    .await
    .expect("send_navigate did not return within 45s")
}

// ── (a) raised budget → slow-but-healthy navigate SUCCEEDS ──────────────────

#[tokio::test]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn navigate_succeeds_when_budget_raised_above_delay() {
    assert_binaries_built();
    // 5s budget, ~200ms navigate delay → comfortably within budget.
    let (mgr, id, _udd) = make_manager("cdp-timeout-success", "5000");

    let result = navigate(&mgr, id, "http://fake.test/slow/200").await;

    assert!(
        result.is_ok(),
        "navigate within the raised budget must succeed, got: {:?}",
        result.err()
    );

    mgr.shutdown_session("cdp-timeout-success").await;
}

// ── (b) default-ish budget under the delay → typed shim_timeout ─────────────

#[tokio::test]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn navigate_times_out_typed_when_delay_exceeds_budget() {
    assert_binaries_built();
    // 300ms budget, ~2s navigate delay → the per-command timeout fires.
    let (mgr, id, _udd) = make_manager("cdp-timeout-trip", "300");

    let result = navigate(&mgr, id, "http://fake.test/slow/2000").await;

    let err = result.expect_err("navigate exceeding the budget must error");
    // Typed timeout, distinguishable from a broken page.
    assert_eq!(
        err.code,
        LoomErrorCode::ShimTimeout,
        "expected ShimTimeout, got {:?} ({})",
        err.code,
        err.message
    );
    assert_eq!(err.code.as_wire(), "shim_timeout");
    assert_ne!(
        err.code,
        LoomErrorCode::SurfaceTrap,
        "a budget timeout must NOT be conflated with a broken-page SurfaceTrap"
    );

    mgr.shutdown_session("cdp-timeout-trip").await;
}
