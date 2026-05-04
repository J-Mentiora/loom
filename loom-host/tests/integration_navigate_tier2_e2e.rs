//! End-to-end spine test for `navigate-receipt-tier2-still-missing`
//! (anti-Potemkin per Phase 8 round-2 retest skeptic plan-review).
//!
//! Drives `ShimManager::send_navigate` against the real
//! `loom-shim-chromium` binary which spawns `fake-chromium`. The point
//! of this test is to validate the **upstream half** of the wire-receipt
//! plumbing — the data sources that this feature lifts onto the wire
//! receipt:
//!
//!   * `outcome.network_events` → wire `side_effects[]` (AC-NAVRECEIPT2-03)
//!   * `outcome.network_events` → `NetworkSummary` aggregation
//!     (brief AC-NAVRECEIPT2-01 extension)
//!   * `outcome.console_lines`  → wire `console_lines` (brief extension)
//!   * `outcome.dom_bytes` / `outcome.screenshot_bytes` → ContentStore.put →
//!     `dom_snapshot_hash` / `screenshot_after_hash` (AC-NAVRECEIPT2-02)
//!
//! The wire-receipt JSON shape and `--capture-policy minimal` enforcement
//! are covered by in-process tests in
//! `loom-rpc/tests/integration_navigate_tier2_still_missing.rs`. **This**
//! test catches the failure mode that allowed `899357f82` to ship with an
//! empty wire receipt: the shim-side data was always there; the
//! marshalling layer just dropped it. Driving the real shim path here
//! verifies the upstream values exist before they enter that layer.
//!
//! Run:
//!   cargo build -p loom-shims --features fake-chromium-bin --bin fake-chromium
//!   cargo build -p loom-shims --bin loom-shim-chromium
//!   cargo test -p loom-host --test integration_navigate_tier2_e2e -- --ignored
//!
//! Marked `#[ignore]` so a default `cargo test --workspace` doesn't force
//! the fake-chromium build.

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
            "fake-chromium binary not built at {fake_path}; run `cargo build -p loom-shims --features fake-chromium-bin --bin fake-chromium` first"
        );
    }
    if !std::path::Path::new(&shim_path).exists() {
        panic!(
            "loom-shim-chromium binary not built at {shim_path}; run `cargo build -p loom-shims --bin loom-shim-chromium` first"
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

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn navigate_outcome_carries_upstream_inputs_for_wire_receipt() {
    use loom_core::receipt_builder::receipt_builder::NetworkSummary;

    assert_binaries_built();
    let (mgr, id, _udd) = make_manager("tier2-spine-200");

    let outcome = tokio::time::timeout(
        Duration::from_secs(45),
        mgr.send_navigate(
            id.clone(),
            "test-action".to_string(),
            0,
            0,
            "http://fake.test/status/200".into(),
            30_000,
            loom_shared::types::Seed(0),
            loom_shared::types::EpochMs(0),
            true,
        ),
    )
    .await
    .expect("send_navigate timed out")
    .expect("send_navigate returned an error");

    // (1) AC-NAVRECEIPT2-03 precondition: shim DOES surface network_events.
    // 899357f82's wire receipt had `side_effects: vec![]` — that was the
    // marshalling layer dropping data the shim already produced. Assert
    // the shim's data is present so a future regression that empties
    // network_events here fails loudly rather than silently.
    assert!(
        !outcome.network_events.is_empty(),
        "fake-chromium must emit at least one Network.responseReceived for status/200"
    );
    let doc = outcome
        .network_events
        .iter()
        .find(|e| e.error_reason.is_none())
        .expect("expected a non-error network event");
    assert_eq!(doc.status, 200);
    assert_eq!(doc.method, "GET");
    assert_eq!(doc.url, "http://fake.test/status/200");

    // (2) Brief AC-NAVRECEIPT2-01 extension precondition: outcome.console_lines
    // is the structural source for the wire `console_lines` field. Phase 6
    // shim returns Vec::new(); this assertion just pins the type so a
    // future shim that emits console events gets caught by the wire-receipt
    // tests, not silently dropped.
    let _: &Vec<loom_shared::navigate_outcome::ShimConsoleLine> = &outcome.console_lines;

    // (3) Brief AC-NAVRECEIPT2-01 extension: NetworkSummary aggregation
    // logic mirroring host_function_table::navigate_execute. Verifies the
    // summary computation against real shim output.
    let summary = NetworkSummary {
        total_count: outcome.network_events.len() as u64,
        total_bytes: outcome
            .network_events
            .iter()
            .map(|e| e.response_bytes)
            .sum(),
        error_count: outcome
            .network_events
            .iter()
            .filter(|e| e.status >= 400 || e.error_reason.is_some())
            .count() as u64,
    };
    assert_eq!(
        summary.error_count, 0,
        "200 navigate must aggregate to zero errors"
    );
    assert!(
        summary.total_count >= 1,
        "summary.total_count must reflect at least the document load"
    );

    // (4) AC-NAVRECEIPT2-02 precondition: dom + screenshot bytes exist for
    // ContentStore.put. The actual put + 64-char SHA-256 hex assertion
    // happens in the in-process integration test (loom-rpc/tests/...);
    // here we just verify the shim produced bytes (or an explicit empty
    // marker) — silent absence is the failure mode this test catches.
    assert!(
        !outcome.dom_bytes.is_empty() || !outcome.dom_after_sha256.is_empty(),
        "either dom_bytes is populated or dom_after_sha256 is set; both empty = shim regression"
    );
    assert!(
        !outcome.screenshot_bytes.is_empty() || !outcome.screenshot_sha256.is_empty(),
        "either screenshot_bytes is populated or screenshot_sha256 is set"
    );

    mgr.shutdown_session("tier2-spine-200").await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn error_navigate_summary_counts_4xx_as_error() {
    use loom_core::receipt_builder::receipt_builder::NetworkSummary;

    assert_binaries_built();
    let (mgr, id, _udd) = make_manager("tier2-spine-404");

    let outcome = tokio::time::timeout(
        Duration::from_secs(45),
        mgr.send_navigate(
            id.clone(),
            "test-action".to_string(),
            0,
            0,
            "http://fake.test/status/404".into(),
            30_000,
            loom_shared::types::Seed(0),
            loom_shared::types::EpochMs(0),
            true,
        ),
    )
    .await
    .expect("send_navigate timed out")
    .expect("send_navigate returned an error");

    let summary = NetworkSummary {
        total_count: outcome.network_events.len() as u64,
        total_bytes: outcome
            .network_events
            .iter()
            .map(|e| e.response_bytes)
            .sum(),
        error_count: outcome
            .network_events
            .iter()
            .filter(|e| e.status >= 400 || e.error_reason.is_some())
            .count() as u64,
    };
    assert!(
        summary.error_count >= 1,
        "404 navigate must aggregate to at least one error in NetworkSummary"
    );

    mgr.shutdown_session("tier2-spine-404").await;
}
