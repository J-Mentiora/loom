//! End-to-end blocklist enforcement test.
//!
//! Drives `ShimManager::send_navigate` against the real
//! `loom-shim-chromium` binary, which spawns the test-only
//! `fake-chromium` binary. fake-chromium pattern-matches the navigate
//! URL `http://fake.test/page-with-tracker` and synthesizes:
//!
//!   1. `Fetch.requestPaused` for the document URL (top-frame skip-gate)
//!   2. `Fetch.requestPaused` for `https://www.google-analytics.com/analytics.js`
//!      (sub-resource matching the default blocklist)
//!   3. `Network.responseReceived` for the document
//!   4. `Page.loadEventFired`
//!
//! This test verifies the live wire path:
//!   - Shim's `Fetch.enable` was issued (otherwise no Fetch events
//!     would ever arrive).
//!   - Shim's interceptor recorded the GA event as a `BlockedEvent`.
//!   - `NavigateOutcome.blocked_events` carries the BlockedEvent
//!     across the CBOR wire to the host.
//!
//! Run:
//!   cargo build -p loom-shims --features fake-chromium-bin --bin fake-chromium
//!   cargo build -p loom-cli --bin loom-shim-chromium
//!   cargo test -p loom-host --test integration_blocklist_e2e -- --ignored
//!
//! Marked `#[ignore]` so a default `cargo test --workspace` doesn't
//! force the fake-chromium build.

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
    let fake_path = fake_chromium_bin();
    let shim_path = shim_bin();
    if !std::path::Path::new(&fake_path).exists() {
        panic!(
            "fake-chromium binary not built at {fake_path}; \
             run `cargo build -p loom-shims --features fake-chromium-bin --bin fake-chromium` first"
        );
    }
    if !std::path::Path::new(&shim_path).exists() {
        panic!(
            "loom-shim-chromium binary not built at {shim_path}; \
             run `cargo build -p loom-cli --bin loom-shim-chromium` first"
        );
    }
}

fn make_manager(session_label: &str) -> (std::sync::Arc<ShimManager>, ShimId, tempfile::TempDir) {
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

// ── Blocklist sub-resource is blocked and recorded ─────────────────────────

/// Drive a full PageNavigate through the real shim binary; assert the
/// returned `NavigateOutcome` carries a `BlockedEvent` for the GA
/// sub-resource that fake-chromium synthesized.
#[tokio::test]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn blocklist_ga_subresource_is_blocked_and_recorded() {
    assert_binaries_built();
    let (mgr, id, _udd) = make_manager("blocklist-05-ga");

    let outcome = tokio::time::timeout(
        Duration::from_secs(45),
        mgr.send_navigate(
            id.clone(),
            "test-blocklist-action".to_string(),
            0,
            0,
            "http://fake.test/page-with-tracker".to_string(),
            30_000,
            loom_shared::types::Seed(0),
            loom_shared::types::EpochMs(0),
            true, // blocklist_enabled
        ),
    )
    .await
    .expect("send_navigate did not return within 45s")
    .expect("send_navigate returned an error");

    // The fake-chromium harness emits exactly one blocklisted
    // sub-resource (GA analytics.js). The interceptor must record it.
    assert_eq!(
        outcome.blocked_events.len(),
        1,
        "expected exactly one BlockedEvent for the GA sub-resource; got {:?}",
        outcome.blocked_events
    );
    let blocked = &outcome.blocked_events[0];
    assert_eq!(blocked.url, "https://www.google-analytics.com/analytics.js");
    assert_eq!(
        blocked.reason, "analytics",
        "reason must equal the lowercased section header from default_blocklist.txt"
    );
    assert_eq!(
        blocked.matched_pattern, "*.google-analytics.com",
        "matched_pattern must be the wildcard entry"
    );

    mgr.shutdown_session("blocklist-05-ga").await;
}

/// With `blocklist_enabled = false`, the same fake-chromium fixture
/// produces ZERO BlockedEvents because the shim never issues
/// `Fetch.enable` (no interception → no Fetch.* events → no gate
/// decisions).
#[tokio::test]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn no_blocklist_disables_enforcement() {
    assert_binaries_built();
    let (mgr, id, _udd) = make_manager("blocklist-04-disabled");

    let outcome = tokio::time::timeout(
        Duration::from_secs(45),
        mgr.send_navigate(
            id.clone(),
            "test-blocklist-action".to_string(),
            0,
            0,
            "http://fake.test/page-with-tracker".to_string(),
            30_000,
            loom_shared::types::Seed(0),
            loom_shared::types::EpochMs(0),
            false, // blocklist_enabled — disabled per --no-blocklist
        ),
    )
    .await
    .expect("send_navigate did not return within 45s")
    .expect("send_navigate returned an error");

    assert!(
        outcome.blocked_events.is_empty(),
        "with blocklist_enabled=false, no BlockedEvents may be recorded; got {:?}",
        outcome.blocked_events
    );

    mgr.shutdown_session("blocklist-04-disabled").await;
}
