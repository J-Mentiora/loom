//! End-to-end spine test for the navigate-receipt structural fields:
//! catches regressions where the marshalling layer drops shim-side data
//! before it reaches the wire receipt.
//!
//! Drives `ShimManager::send_navigate` against the real
//! `loom-shim-chromium` binary which spawns `fake-chromium`. The point
//! of this test is to validate the **upstream half** of the wire-receipt
//! plumbing — the data sources that this feature lifts onto the wire
//! receipt:
//!
//!   * `outcome.network_events` → wire `side_effects[]`
//!   * `outcome.network_events` → `NetworkSummary` aggregation
//!     (brief  extension)
//!   * `outcome.console_lines`  → wire `console_lines` (brief extension)
//!   * `outcome.dom_bytes` / `outcome.screenshot_bytes` → ContentStore.put →
//!     `dom_snapshot_hash` / `screenshot_after_hash`
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
//!   cargo build -p loom-cli --bin loom-shim-chromium
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

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn navigate_outcome_carries_upstream_inputs_for_wire_receipt() {
    use loom_core::receipt_builder::receipt_builder::NetworkSummary;

    assert_binaries_built();
    let (mgr, id, _udd) = make_manager("tier2-spine-200");

    let outcome = tokio::time::timeout(
        Duration::from_secs(45),
        mgr.send_navigate(loom_host::shim_manager::SendNavigateParams {
            id: id.clone(),
            action_id: "test-action".to_string(),
            session_id: 0,
            target_id: 0,
            url: "http://fake.test/status/200".into(),
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
    .expect("send_navigate timed out")
    .expect("send_navigate returned an error");

    // (1)  precondition: shim DOES surface network_events.
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
    // network-accounting-har: the Document hashed event now carries the real
    // HTTP method (backfilled from the correlated
    // Network.requestWillBeSent.request.method) and the on-wire response size
    // (from Network.loadingFinished.encodedDataLength — no getResponseBody
    // round-trip). fake-chromium emits loadingFinished{encodedDataLength:1234}
    // for the /status/200 document, so the captured size is exactly 1234.
    assert_eq!(
        doc.method, "GET",
        "method must be backfilled from the correlated requestWillBeSent"
    );
    assert_eq!(
        doc.response_bytes, 1234,
        "response_bytes must come from Network.loadingFinished.encodedDataLength"
    );
    assert_eq!(doc.url, "http://fake.test/status/200");

    // (2) Precondition: outcome.console_lines is the structural source for
    // the wire `console_lines` field. The current shim returns Vec::new();
    // this assertion just pins the type so a future shim that emits
    // console events gets caught by the wire-receipt tests, not silently
    // dropped.
    let _: &Vec<loom_shared::navigate_outcome::ShimConsoleLine> = &outcome.console_lines;

    // (2b) settle-capture: the capture was gated on the default `settled`
    // readiness state and the fake page settles cleanly, so the shim must
    // surface `settled` / `reached` through to the NavigateOutcome. A
    // regression that drops the readiness gate (capturing at commit time)
    // would leave these unset.
    assert_eq!(
        outcome.settle_until, "settled",
        "navigate default must gate capture on settled readiness"
    );
    assert_eq!(
        outcome.settle_outcome, "reached",
        "the fake page settles cleanly → settle_outcome must be `reached`"
    );

    // (3) NetworkSummary aggregation
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
    // network-accounting-har: total_bytes is the sum of response_bytes over the
    // Document-only network_events. Only the document (encodedDataLength=1234)
    // contributes — the xhr lives in network_entries, not network_events — so
    // the wire summary's total_bytes is exactly the document size.
    assert_eq!(
        summary.total_bytes, 1234,
        "total_bytes must reflect the document's on-wire response size"
    );

    // (4)  precondition: dom + screenshot bytes exist for
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

    // REGRESSION (mcp-screenshot-delivery): the bytes navigate stores into the
    // content store (host_impl.rs `content_store.put(&outcome.screenshot_bytes)`)
    // MUST be a RAW PNG, not the CBOR `{data:<base64-PNG>}` envelope the shim
    // returns. Storing the envelope is exactly the "screenshots arrive empty at
    // MCP clients" bug: the cas blob resolves to unrenderable CBOR. This pins the
    // `action_executor` decode so the double-encoding can't silently come back.
    assert!(
        loom_shared::screenshot_decode::is_png(&outcome.screenshot_bytes),
        "navigate screenshot_bytes must be a raw PNG (magic 89 50 4E 47); got first bytes {:02x?} \
         — a CBOR/base64 envelope here means the double-encoding bug regressed",
        &outcome.screenshot_bytes[..outcome.screenshot_bytes.len().min(8)]
    );
    // Belt-and-suspenders: the stored bytes must NOT decode as a CBOR map with a
    // `data` field (the pre-fix stored form). decode_cdp_screenshot succeeds on
    // the OLD envelope and fails on a raw PNG, so failure here == correctly fixed.
    assert!(
        loom_shared::screenshot_decode::decode_cdp_screenshot(&outcome.screenshot_bytes).is_err(),
        "navigate screenshot_bytes must NOT be a decodable CBOR{{data:base64}} envelope"
    );

    // (5) mcp-network-entries AC: the full-capture network_entries path
    // surfaces the xhr to `/api/thing` — which the Document-only
    // `network_events` path drops — with method + status + resource_type.
    // This is the studio's per-test-route-footprint payload.
    let api = outcome
        .network_entries
        .iter()
        .find(|e| e.url.ends_with("/api/thing"))
        .expect("network_entries must contain the xhr to /api/thing");
    assert_eq!(api.method, "GET", "method comes from requestWillBeSent");
    assert_eq!(api.status, 200, "status comes from responseReceived");
    assert_eq!(api.resource_type, "XHR", "resource_type is the CDP type");
    assert!(
        !api.request_id.is_empty(),
        "request_id correlates the events"
    );
    // The xhr is NOT in the Document-only network_events (count unchanged).
    assert!(
        !outcome
            .network_events
            .iter()
            .any(|e| e.url.ends_with("/api/thing")),
        "network_events (Document-only, hashed) must NOT contain the xhr"
    );

    // (6) network_log tool path: reading the accumulator (NON-draining) after
    // the navigate returns the same session-accumulated entries — the document
    // load AND the xhr — proving the loom.web.network_log backing path.
    let log = mgr
        .send_network_log(id.clone(), 0, 0)
        .await
        .expect("send_network_log returned an error");
    assert!(
        log.network_entries
            .iter()
            .any(|e| e.url.ends_with("/api/thing") && e.method == "GET" && e.status == 200),
        "network_log must return the session-accumulated xhr entry"
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
        mgr.send_navigate(loom_host::shim_manager::SendNavigateParams {
            id: id.clone(),
            action_id: "test-action".to_string(),
            session_id: 0,
            target_id: 0,
            url: "http://fake.test/status/404".into(),
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

/// Helper: drive one navigation through the real shim and return its
/// (normalized) DOM-snapshot bytes — what the host hashes into
/// `dom_snapshot_hash` via `content_store.put`.
async fn navigate_dom_bytes(session_label: &str) -> Vec<u8> {
    let (mgr, id, _udd) = make_manager(session_label);
    let outcome = tokio::time::timeout(
        Duration::from_secs(45),
        mgr.send_navigate(loom_host::shim_manager::SendNavigateParams {
            id: id.clone(),
            action_id: "test-action".to_string(),
            session_id: 0,
            target_id: 0,
            url: "http://fake.test/status/200".into(),
            budget_ms: 30_000,
            seed: loom_shared::types::Seed(42),
            epoch_ms: loom_shared::types::EpochMs(0),
            blocklist_enabled: true,
            until: "settled".to_string(),
            determinism_enabled: true,
            audio_enabled: false,
        }),
    )
    .await
    .expect("send_navigate timed out")
    .expect("send_navigate returned an error");
    mgr.shutdown_session(session_label).await;
    outcome.dom_bytes
}

/// stable-dom-snapshot-hash regression (Part 1): two INDEPENDENT same-seed
/// navigations of byte-identical content must produce an IDENTICAL
/// `dom_snapshot_hash`. fake-chromium emits a fresh ephemeral `frameId` per
/// call/process, so this can only hold because the shim's `dom_normalize` strips
/// it from the hashed/stored DOM bytes. Before the fix the two runs differed by
/// exactly that `frameId`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn two_independent_navigations_yield_identical_dom_snapshot_bytes() {
    assert_binaries_built();
    let a = navigate_dom_bytes("xrun-dom-a").await;
    let b = navigate_dom_bytes("xrun-dom-b").await;
    assert!(!a.is_empty(), "dom bytes must be populated");
    assert_eq!(
        a, b,
        "two independent same-seed navigations of identical content must yield \
         byte-equal normalized DOM (→ identical dom_snapshot_hash); a difference \
         means an ephemeral CDP id (e.g. frameId) leaked into the hashed bytes"
    );
}
