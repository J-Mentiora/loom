//! End-to-end integration test for AC-NAVERR-01..05.
//!
//! Drives `ShimManager::send_navigate` against the real
//! `loom-shim-chromium` binary, which spawns the test-only
//! `fake-chromium` binary. fake-chromium pattern-matches the navigate
//! URL and emits synthetic CDP events:
//!
//!   `http://fake.test/status/<N>`  → Network.responseReceived (type=Document, status=N)
//!   `http://fake.test/error/<CDP>` → Network.loadingFailed (errorText=CDP) +
//!                                    Page.navigate response.errorText=CDP
//!
//! This is the AC-NAVERR-05 fitness function: the prior fix only had
//! manifest-level tests with hand-crafted ReceiptBuilder fixtures, so
//! the live CDP-event ingestion path was never validated. This test
//! covers it.
//!
//! Run:
//!   cargo build -p loom-shims --features fake-chromium-bin --bin fake-chromium
//!   cargo build -p loom-cli --bin loom-shim-chromium
//!   cargo test -p loom-host --test integration_navigate_status_codes -- --ignored
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
            "fake-chromium binary not built at {fake_path}; run `cargo build -p loom-shims --features fake-chromium-bin --bin fake-chromium` first"
        );
    }
    if !std::path::Path::new(&shim_path).exists() {
        panic!(
            "loom-shim-chromium binary not built at {shim_path}; run `cargo build -p loom-cli --bin loom-shim-chromium` first"
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

async fn navigate(
    mgr: &std::sync::Arc<ShimManager>,
    id: ShimId,
    url: &str,
) -> loom_shared::navigate_outcome::NavigateOutcome {
    tokio::time::timeout(
        Duration::from_secs(45),
        mgr.send_navigate(
            id,
            "test-action".to_string(),
            0,
            0,
            url.to_string(),
            30_000,
            loom_shared::types::Seed(0),
            loom_shared::types::EpochMs(0),
            true,
        ),
    )
    .await
    .expect("send_navigate did not return within 45s")
    .expect("send_navigate returned an error")
}

// ── AC-NAVERR-04: 200 → success path with status_code populated ─────────────

#[tokio::test]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn ac_naverr_04_status_200_propagates_status_code() {
    assert_binaries_built();
    let (mgr, id, _udd) = make_manager("naverr-04-200");

    let outcome = navigate(&mgr, id.clone(), "http://fake.test/status/200").await;

    assert_eq!(
        outcome.status_code, 200,
        "AC-NAVERR-04: status_code must be 200 for a successful 200 navigate"
    );
    let doc = outcome
        .network_events
        .iter()
        .find(|e| e.error_reason.is_none())
        .expect("AC-NAVERR-04: a network event must be present");
    assert_eq!(doc.status, 200u16);
    assert_eq!(doc.url, "http://fake.test/status/200");

    mgr.shutdown_session("naverr-04-200").await;
}

// ── AC-NAVERR-01: 404 → status_code surfaces ────────────────────────────────

#[tokio::test]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn ac_naverr_01_status_404_propagates_status_code() {
    assert_binaries_built();
    let (mgr, id, _udd) = make_manager("naverr-01-404");

    let outcome = navigate(&mgr, id.clone(), "http://fake.test/status/404").await;

    assert_eq!(
        outcome.status_code, 404,
        "AC-NAVERR-01: status_code must be 404 — this is the regression that the prior fix missed"
    );
    let doc = outcome
        .network_events
        .iter()
        .find(|e| e.status == 404)
        .expect("AC-NAVERR-01: a Document network event with status=404 must be present");
    assert_eq!(doc.status, 404u16);
    assert_eq!(doc.url, "http://fake.test/status/404");
    assert_eq!(doc.error_reason, None);

    mgr.shutdown_session("naverr-01-404").await;
}

// ── AC-NAVERR-02: 500 → status_code surfaces ────────────────────────────────

#[tokio::test]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn ac_naverr_02_status_500_propagates_status_code() {
    assert_binaries_built();
    let (mgr, id, _udd) = make_manager("naverr-02-500");

    let outcome = navigate(&mgr, id.clone(), "http://fake.test/status/500").await;

    assert_eq!(
        outcome.status_code, 500,
        "AC-NAVERR-02: status_code must be 500"
    );
    let doc = outcome
        .network_events
        .iter()
        .find(|e| e.status == 500)
        .expect("AC-NAVERR-02: a Document network event with status=500 must be present");
    assert_eq!(doc.status, 500u16);

    mgr.shutdown_session("naverr-02-500").await;
}

// ── AC-NAVERR-03: DNS failure → error_reason + error_kind classified ────────

#[tokio::test]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn ac_naverr_03_dns_failure_emits_classified_error_event() {
    assert_binaries_built();
    let (mgr, id, _udd) = make_manager("naverr-03-dns");

    let outcome = navigate(
        &mgr,
        id.clone(),
        "http://fake.test/error/ERR_NAME_NOT_RESOLVED",
    )
    .await;

    let err_event = outcome
        .network_events
        .iter()
        .find(|e| e.error_reason.is_some())
        .expect("AC-NAVERR-03: a network event with error_reason must be present");
    assert!(
        err_event
            .error_reason
            .as_deref()
            .unwrap()
            .contains("ERR_NAME_NOT_RESOLVED"),
        "AC-NAVERR-03: error_reason must carry the chromium error code (got {:?})",
        err_event.error_reason
    );
    assert_eq!(
        err_event.error_kind.as_deref(),
        Some("dns_failure"),
        "AC-NAVERR-03: error_kind must be 'dns_failure'"
    );

    mgr.shutdown_session("naverr-03-dns").await;
}

// ── AC-NAVERR-03 (extension): connect-refused classifies separately ─────────

#[tokio::test]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn ac_naverr_03_connect_refused_classified_separately() {
    assert_binaries_built();
    let (mgr, id, _udd) = make_manager("naverr-03-conn-refused");

    let outcome = navigate(
        &mgr,
        id.clone(),
        "http://fake.test/error/ERR_CONNECTION_REFUSED",
    )
    .await;

    let err_event = outcome
        .network_events
        .iter()
        .find(|e| e.error_reason.is_some())
        .expect("AC-NAVERR-03 (ext): a network event with error_reason must be present");
    assert_eq!(
        err_event.error_kind.as_deref(),
        Some("connect_refused"),
        "AC-NAVERR-03 (ext): error_kind must be 'connect_refused' for ERR_CONNECTION_REFUSED"
    );

    mgr.shutdown_session("naverr-03-conn-refused").await;
}
