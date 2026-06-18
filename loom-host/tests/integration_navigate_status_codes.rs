//! End-to-end integration test for typed navigate-error receipts.
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
//! This is the fitness function for the live CDP-event ingestion path.
//! Prior coverage had only manifest-level tests with hand-crafted
//! ReceiptBuilder fixtures, so the ingestion path itself was never
//! validated. This test covers it.
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
        mgr.send_navigate(loom_host::shim_manager::SendNavigateParams {
            id,
            action_id: "test-action".to_string(),
            session_id: 0,
            target_id: 0,
            url: url.to_string(),
            budget_ms: 30_000,
            seed: loom_shared::types::Seed(0),
            epoch_ms: loom_shared::types::EpochMs(0),
            blocklist_enabled: true,
            until: "settled".to_string(),
            determinism_enabled: true,
        }),
    )
    .await
    .expect("send_navigate did not return within 45s")
    .expect("send_navigate returned an error")
}

// ── 200 → success path with status_code populated ──────────────────────────

#[tokio::test]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn naverr_status_200_propagates_status_code() {
    assert_binaries_built();
    let (mgr, id, _udd) = make_manager("naverr-200");

    let outcome = navigate(&mgr, id.clone(), "http://fake.test/status/200").await;

    assert_eq!(
        outcome.status_code, 200,
        "status_code must be 200 for a successful 200 navigate"
    );
    let doc = outcome
        .network_events
        .iter()
        .find(|e| e.error_reason.is_none())
        .expect("a network event must be present");
    assert_eq!(doc.status, 200u16);
    assert_eq!(doc.url, "http://fake.test/status/200");

    mgr.shutdown_session("naverr-200").await;
}

// ── 404 → status_code surfaces ─────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn naverr_status_404_propagates_status_code() {
    assert_binaries_built();
    let (mgr, id, _udd) = make_manager("naverr-404");

    let outcome = navigate(&mgr, id.clone(), "http://fake.test/status/404").await;

    assert_eq!(
        outcome.status_code, 404,
        "status_code must be 404 — this is the regression that the prior fix missed"
    );
    let doc = outcome
        .network_events
        .iter()
        .find(|e| e.status == 404)
        .expect("a Document network event with status=404 must be present");
    assert_eq!(doc.status, 404u16);
    assert_eq!(doc.url, "http://fake.test/status/404");
    assert_eq!(doc.error_reason, None);

    mgr.shutdown_session("naverr-404").await;
}

// ── 500 → status_code surfaces ─────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn naverr_status_500_propagates_status_code() {
    assert_binaries_built();
    let (mgr, id, _udd) = make_manager("naverr-500");

    let outcome = navigate(&mgr, id.clone(), "http://fake.test/status/500").await;

    assert_eq!(outcome.status_code, 500, "status_code must be 500");
    let doc = outcome
        .network_events
        .iter()
        .find(|e| e.status == 500)
        .expect("a Document network event with status=500 must be present");
    assert_eq!(doc.status, 500u16);

    mgr.shutdown_session("naverr-500").await;
}

// ── DNS failure → error_reason + error_kind classified ─────────────────────

#[tokio::test]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn naverr_dns_failure_emits_classified_error_event() {
    assert_binaries_built();
    let (mgr, id, _udd) = make_manager("naverr-dns");

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
        .expect("a network event with error_reason must be present");
    assert!(
        err_event
            .error_reason
            .as_deref()
            .unwrap()
            .contains("ERR_NAME_NOT_RESOLVED"),
        "error_reason must carry the chromium error code (got {:?})",
        err_event.error_reason
    );
    assert_eq!(
        err_event.error_kind.as_deref(),
        Some("dns_failure"),
        "error_kind must be 'dns_failure'"
    );

    mgr.shutdown_session("naverr-dns").await;
}

// ── Iframe 404 must not contaminate the main document's verdict ────────────

#[tokio::test]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn naverr_iframe_404_does_not_contaminate_main_status() {
    assert_binaries_built();
    let (mgr, id, _udd) = make_manager("naverr-iframe-404");

    let outcome = navigate(&mgr, id.clone(), "http://fake.test/page-with-iframe-404").await;

    assert_eq!(
        outcome.status_code, 200,
        "status_code must be the MAIN document's 200, not the iframe's 404"
    );
    let main_idx = outcome
        .main_document_event_index
        .expect("main_document_event_index must be set when the main document responded");
    let main_ev = &outcome.network_events[main_idx as usize];
    assert_eq!(main_ev.status, 200u16);
    assert_eq!(main_ev.url, "http://fake.test/page-with-iframe-404");
    // The iframe's 404 stays in network_events for observability.
    let iframe_ev = outcome
        .network_events
        .iter()
        .find(|e| e.status == 404)
        .expect("iframe Document 404 event must be retained for observability");
    assert_eq!(iframe_ev.url, "http://fake.test/embedded-iframe-404");

    mgr.shutdown_session("naverr-iframe-404").await;
}

// ── Stale Document events must not leak into the next navigate ─────────────

#[tokio::test]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn naverr_stale_document_events_do_not_leak_into_next_navigate() {
    assert_binaries_built();
    let (mgr, id, _udd) = make_manager("naverr-stale-leak");

    // Establish a page first (also lazy-spawns the target).
    let first = navigate(&mgr, id.clone(), "http://fake.test/status/200").await;
    assert_eq!(first.status_code, 200);

    // Simulate an in-session CLICK that triggers a real link navigation:
    // the evaluate sentinel makes fake-chromium emit a Document
    // requestWillBeSent + responseReceived(404) OUTSIDE any navigate.
    // Those events land in the shim's hashed accumulator with no drain —
    // exactly the contamination window from the audit.
    let eval = tokio::time::timeout(
        Duration::from_secs(45),
        mgr.send_evaluate(loom_host::shim_manager::SendEvaluateParams {
            id: id.clone(),
            action_id: "test-action".to_string(),
            session_id: 0,
            target_id: 0,
            expression: "__loom_test_emit_doc_event__:404".to_string(),
            budget_ms: 30_000,
            seed: loom_shared::types::Seed(0),
            epoch_ms: loom_shared::types::EpochMs(0),
            determinism_enabled: true,
        }),
    )
    .await
    .expect("send_evaluate did not return within 45s");
    assert!(
        eval.is_ok(),
        "stale-event injection evaluate failed: {eval:?}"
    );

    // Next navigate: must see ONLY its own events — the stale 404 from the
    // click-induced load is discarded at navigate START (clear_events), so
    // it cannot drive status_code or the host's failure verdict.
    let outcome = navigate(&mgr, id.clone(), "http://fake.test/status/200").await;

    assert_eq!(
        outcome.status_code, 200,
        "stale Document events from before the navigate must not drive status_code"
    );
    assert!(
        !outcome.network_events.iter().any(|e| e.status == 404),
        "stale 404 event leaked into the next navigate's network_events: {:?}",
        outcome.network_events
    );
    let main_idx = outcome
        .main_document_event_index
        .expect("main_document_event_index must be set");
    assert_eq!(outcome.network_events[main_idx as usize].status, 200u16);

    mgr.shutdown_session("naverr-stale-leak").await;
}

// ── Connect-refused classifies separately ──────────────────────────────────

#[tokio::test]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn naverr_connect_refused_classified_separately() {
    assert_binaries_built();
    let (mgr, id, _udd) = make_manager("naverr-conn-refused");

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
        .expect("a network event with error_reason must be present");
    assert_eq!(
        err_event.error_kind.as_deref(),
        Some("connect_refused"),
        "error_kind must be 'connect_refused' for ERR_CONNECTION_REFUSED"
    );

    mgr.shutdown_session("naverr-conn-refused").await;
}
