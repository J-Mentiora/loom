//! End-to-end proof of the standalone `web.wait_for` readiness verb
//! (settle-capture slice 2) through the REAL shim path (host →
//! `ShimManager::send_wait_for` → `loom-shim-chromium` → `fake-chromium`).
//!
//! `wait_for` reuses the exact SettleDriver / ReadinessMonitor the navigate
//! gate uses, but with NO navigation and NO capture — it waits on the current
//! page and returns only the settle verdict. These cases drive the wiring with
//! `LOOM_FAKE_CHROMIUM_SCRIPT` (see `fake-chromium.rs`) and assert the verdict
//! reaches `WaitOutcome` end-to-end:
//!
//!   - clean page → `reached` (the default fake page settles immediately).
//!   - never-settles (network): perpetual in-flight > idle threshold → typed
//!     `timeout`, `network_count_at_settle >= 3`, no hang.
//!   - never-settles (DOM): perpetual mutations, all else quiet → typed
//!     `dom_unstable`, distinct from the network timeout.
//!
//! Run:
//!   cargo build -p loom-shims --features fake-chromium-bin --bin fake-chromium
//!   cargo build -p loom-cli --bin loom-shim-chromium
//!   cargo test -p loom-host --test integration_wait_for_e2e -- --ignored

#![cfg(unix)]

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
    for (path, build) in [
        (
            fake_chromium_bin(),
            "cargo build -p loom-shims --features fake-chromium-bin --bin fake-chromium",
        ),
        (
            shim_bin(),
            "cargo build -p loom-cli --bin loom-shim-chromium",
        ),
    ] {
        if !std::path::Path::new(&path).exists() {
            panic!("missing binary {path}; run `{build}` first");
        }
    }
}

/// Build a ShimManager whose `fake-chromium` is driven by `script_json`.
fn make_manager_with_script(
    session_label: &str,
    script_json: &str,
) -> (std::sync::Arc<ShimManager>, ShimId, tempfile::TempDir) {
    let user_data_dir = tempfile::tempdir().expect("tempdir");
    let script_path = user_data_dir.path().join("settle_script.json");
    std::fs::write(&script_path, script_json).expect("write settle script");

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
                (
                    "LOOM_FAKE_CHROMIUM_SCRIPT".into(),
                    script_path.display().to_string(),
                ),
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

async fn wait_for_settled(
    mgr: &std::sync::Arc<ShimManager>,
    id: &ShimId,
) -> loom_shared::navigate_outcome::WaitOutcome {
    // 180s outer guard: the never-settles cases walk the full 2000-tick
    // ceiling, whose wall-clock (ceiling × per-tick CDP round-trip) can
    // approach ~30s on a slow CI runner. The guard only exists to fail loudly
    // on a TRUE infinite hang, so it needs ample headroom. The clean case
    // settles in ~25ms and never approaches it.
    tokio::time::timeout(
        Duration::from_secs(180),
        mgr.send_wait_for(
            id.clone(),
            "test-action".to_string(),
            0,
            0,
            "settled".to_string(),
            30_000,
            loom_shared::types::Seed(0),
            loom_shared::types::EpochMs(0),
            true,
        ),
    )
    .await
    .expect("send_wait_for timed out (a never-settles case must return a TYPED verdict, not hang)")
    .expect("send_wait_for returned an error")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn wait_for_clean_page_reaches_settled() {
    assert_binaries_built();
    // Default script → page is immediately complete, stable, quiet.
    let script = r#"{ "settle_probe": [[true, "http://fake.test/app", 0]] }"#;
    let (mgr, id, _udd) = make_manager_with_script("waitfor-reached", script);

    let outcome = wait_for_settled(&mgr, &id).await;

    assert_eq!(outcome.settle_until, "settled");
    assert_eq!(
        outcome.settle_outcome, "reached",
        "a clean current page must settle to reached"
    );
    // wait_for runs the quiet window (>= 5 ticks) before declaring settled.
    assert!(
        outcome.settle_ms >= 25,
        "got settle_ms={}",
        outcome.settle_ms
    );

    mgr.shutdown_session("waitfor-reached").await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn wait_for_never_settles_network_times_out() {
    assert_binaries_built();
    let script = r#"{
        "settle_probe": [[true, "http://fake.test/poll", 0]],
        "perpetual_inflight": 3
    }"#;
    let (mgr, id, _udd) = make_manager_with_script("waitfor-net-timeout", script);

    let outcome = wait_for_settled(&mgr, &id).await;

    assert_eq!(
        outcome.settle_outcome, "timeout",
        "persistent in-flight requests must hit the bounded network timeout"
    );
    assert!(
        outcome.network_count_at_settle >= 3,
        "got network_count_at_settle={}",
        outcome.network_count_at_settle
    );

    mgr.shutdown_session("waitfor-net-timeout").await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn wait_for_never_settles_dom_is_dom_unstable() {
    assert_binaries_built();
    let script = r#"{ "settle_probe": [[true, "http://fake.test/anim", 4]] }"#;
    let (mgr, id, _udd) = make_manager_with_script("waitfor-dom-unstable", script);

    let outcome = wait_for_settled(&mgr, &id).await;

    assert_eq!(
        outcome.settle_outcome, "dom_unstable",
        "a perpetually-mutating DOM (all else quiet) must report dom_unstable"
    );

    mgr.shutdown_session("waitfor-dom-unstable").await;
}
