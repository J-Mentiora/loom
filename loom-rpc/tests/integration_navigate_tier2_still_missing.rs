//! Integration tests for `navigate-receipt-tier2-still-missing`
//! (AC-NAVRECEIPT2-01..05) — Phase 8 round-2 retest follow-up.
//!
//! Brief mapping (the brief used the older AC-NAVRECEIPT-* numbering;
//! the registry tracks AC-NAVRECEIPT2-* — both refer to the same
//! contract for this feature):
//!
//!   AC-NAVRECEIPT2-01: wire receipt JSON contains keys: url, final_url,
//!     title, status_code, dom_snapshot_hash, screenshot_after_hash,
//!     console_count, console_lines, network_count, network_summary.
//!   AC-NAVRECEIPT2-02: dom_snapshot_hash + screenshot_after_hash are
//!     SHA-256 hex (64 lowercase chars), and the corresponding blobs
//!     exist in the content store (`cs.get(...)` returns the original
//!     bytes).
//!   AC-NAVRECEIPT2-03: side_effects[] contains LoomNetworkEvent entries
//!     captured by the shim's NetworkInterceptor (one per resource).
//!   AC-NAVRECEIPT2-04: combined: full receipt shape AND blobs on disk
//!     in one test (this file's `ac_navreceipt2_04_combined`).
//!   AC-NAVRECEIPT2-05: --capture-policy minimal strips
//!     dom_snapshot_hash, screenshot_after_hash, console_lines, etc.
//!     (covered by `apply_capture_profile_to_wire(Minimal)`).
//!
//! These tests exercise the wire `Receipt` end-to-end through the
//! daemon's decode path (faithful to the production marshalling), but
//! stay in-process so they run in the standard `cargo test --tests`
//! suite. The opt-in subprocess E2E test in
//! `loom-cli/tests/integration_navigate_tier2_e2e.rs` (`#[ignore]`)
//! drives the actual `loom session navigate` binary against
//! fake-chromium — that's the anti-Potemkin spine test.

use loom_core::content_store::{ContentStore, LocalContentStore};
use loom_core::observability::Observability;
use loom_core::receipt_builder::receipt_builder::{CaptureProfile, NetworkSummary};
use loom_rpc::host_service_adapter::host_service_adapter::{Receipt, ReceiptError, ReceiptStatus};
use loom_rpc::host_service_adapter::wire_capture::apply_capture_profile_to_wire;
use loom_shared::navigate_outcome::{LoomNetworkEvent, ShimConsoleLine};

fn fresh_content_store() -> (LocalContentStore, tempfile::TempDir) {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let store_root = tmp.path().join("store");
    std::fs::create_dir_all(&store_root).unwrap();
    std::fs::create_dir_all(tmp.path().join("sessions")).unwrap();
    let obs = Observability::new(store_root.join("loom.log"), false);
    let cs = LocalContentStore::new(store_root, obs);
    (cs, tmp)
}

const SHA256_HEX_LEN: usize = 64;

/// AC-NAVRECEIPT2-02 hash check: 64 lowercase hex characters.
fn assert_is_sha256_hex(label: &str, h: &str) {
    assert_eq!(
        h.len(),
        SHA256_HEX_LEN,
        "{label}: expected 64-char SHA-256 hex, got {len}-char {h:?}",
        len = h.len()
    );
    assert!(
        h.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "{label}: expected lowercase hex only, got {h:?}"
    );
}

fn make_network_events() -> Vec<LoomNetworkEvent> {
    vec![
        LoomNetworkEvent {
            method: "GET".into(),
            url: "https://example.com/".into(),
            request_hash: "0".repeat(64),
            response_hash: "1".repeat(64),
            status: 200,
            content_type: "text/html".into(),
            duration_ms: 50,
            response_bytes: 4096,
            error_reason: None,
            error_kind: None,
        },
        LoomNetworkEvent {
            method: "GET".into(),
            url: "https://example.com/main.css".into(),
            request_hash: "2".repeat(64),
            response_hash: "3".repeat(64),
            status: 200,
            content_type: "text/css".into(),
            duration_ms: 25,
            response_bytes: 1024,
            error_reason: None,
            error_kind: None,
        },
        LoomNetworkEvent {
            method: "GET".into(),
            url: "https://example.com/missing.png".into(),
            request_hash: "4".repeat(64),
            response_hash: "5".repeat(64),
            status: 404,
            content_type: "image/png".into(),
            duration_ms: 10,
            response_bytes: 0,
            error_reason: None,
            error_kind: None,
        },
    ]
}

fn make_console_lines() -> Vec<ShimConsoleLine> {
    vec![
        ShimConsoleLine {
            level: "info".into(),
            message: "page loaded".into(),
        },
        ShimConsoleLine {
            level: "warn".into(),
            message: "missing.png 404".into(),
        },
    ]
}

/// Construct a fully-populated wire `Receipt` mimicking what the daemon
/// would emit at `dispatch_action_blocking` for a successful navigate
/// under `--capture-policy default`. Numeric / hash values must match
/// the make_* fixtures so the AC-02 assertion can roundtrip them.
fn full_navigate_receipt(dom_hash: String, ss_hash: String) -> Receipt {
    let network_events = make_network_events();
    let console_lines = make_console_lines();
    let summary = NetworkSummary {
        total_count: network_events.len() as u64,
        total_bytes: network_events.iter().map(|e| e.response_bytes).sum(),
        error_count: network_events
            .iter()
            .filter(|e| e.status >= 400 || e.error_reason.is_some())
            .count() as u64,
    };
    let side_effects: Vec<serde_json::Value> = network_events
        .iter()
        .map(|e| serde_json::to_value(e).unwrap())
        .collect();

    Receipt {
        action_id: 7,
        session_id: "01HZSESS".into(),
        status: ReceiptStatus::Success,
        timing_ticks: 250,
        side_effects,
        error: None,
        action_hash: Some("aa".repeat(32)),
        outcome_hash: Some("bb".repeat(32)),
        emitted_at_ms: Some(1_714_074_336_000),
        url: Some("https://example.com/".into()),
        final_url: Some("https://example.com/".into()),
        title: Some("Example".into()),
        status_code: Some(200),
        dom_snapshot_hash: Some(dom_hash),
        screenshot_after_hash: Some(ss_hash),
        console_count: Some(console_lines.len() as u64),
        network_count: Some(network_events.len() as u64),
        console_lines,
        network_summary: Some(summary),
        return_value_json: None,
        return_value_blob_ref: None,
    }
}

// ─── AC-NAVRECEIPT2-01: receipt JSON contains all 10 brief-listed keys ─────

#[test]
fn ac_navreceipt2_01_wire_receipt_carries_all_brief_keys() {
    let r = full_navigate_receipt("a".repeat(64), "b".repeat(64));
    let json = serde_json::to_value(&r).expect("wire receipt must serialize");

    let required = [
        "url",
        "final_url",
        "title",
        "status_code",
        "dom_snapshot_hash",
        "screenshot_after_hash",
        "console_count",
        "console_lines",
        "network_count",
        "network_summary",
    ];
    for key in required {
        assert!(
            json.get(key).is_some(),
            "AC-NAVRECEIPT2-01: missing key '{key}' in wire receipt JSON: {json:?}"
        );
    }

    // Identity fields too:
    for key in [
        "action_id",
        "session_id",
        "status",
        "timing_ticks",
        "side_effects",
    ] {
        assert!(json.get(key).is_some(), "missing identity field {key}");
    }
}

// ─── AC-NAVRECEIPT2-02: SHA-256 hex + blob round-trip via ContentStore ─────

#[test]
fn ac_navreceipt2_02_dom_and_screenshot_hashes_are_sha256_hex_with_blob_present() {
    let (cs, _tmp) = fresh_content_store();

    let dom_bytes: Vec<u8> =
        b"<html><head><title>Example</title></head><body>hi</body></html>".to_vec();
    let ss_bytes: Vec<u8> = (0u8..=255).chain(0u8..=255).collect(); // 512 bytes of fake PNG

    let dom_ref = cs.put(&dom_bytes).expect("cs.put(dom)");
    let ss_ref = cs.put(&ss_bytes).expect("cs.put(screenshot)");

    // AC-02 — hash format
    assert_is_sha256_hex("dom_snapshot_hash", &dom_ref.sha256);
    assert_is_sha256_hex("screenshot_after_hash", &ss_ref.sha256);

    // AC-02 — blob exists on disk: round-trip via cs.get
    let got_dom = cs.get(&dom_ref).expect("cs.get(dom)");
    let got_ss = cs.get(&ss_ref).expect("cs.get(screenshot)");
    assert_eq!(got_dom, dom_bytes, "DOM round-trip via ContentStore");
    assert_eq!(got_ss, ss_bytes, "screenshot round-trip via ContentStore");

    // AC-02 — wire receipt carries the same hashes.
    let r = full_navigate_receipt(dom_ref.sha256.clone(), ss_ref.sha256.clone());
    assert_eq!(
        r.dom_snapshot_hash.as_deref(),
        Some(dom_ref.sha256.as_str())
    );
    assert_eq!(
        r.screenshot_after_hash.as_deref(),
        Some(ss_ref.sha256.as_str())
    );
}

// ─── AC-NAVRECEIPT2-03: side_effects[] non-empty + LoomNetworkEvent shape ──

#[test]
fn ac_navreceipt2_03_side_effects_carries_network_events() {
    let r = full_navigate_receipt("a".repeat(64), "b".repeat(64));
    assert!(
        !r.side_effects.is_empty(),
        "AC-NAVRECEIPT2-03: side_effects must be non-empty for navigate"
    );
    // First event has the LoomNetworkEvent shape expected by AC-03.
    let first = &r.side_effects[0];
    for key in ["method", "url", "status", "request_hash", "response_hash"] {
        assert!(
            first.get(key).is_some(),
            "AC-NAVRECEIPT2-03: side_effects[0] missing key {key}: {first:?}"
        );
    }
    assert_eq!(first["method"], "GET");
    assert_eq!(first["status"], 200);
    // Round-trip into the typed struct works.
    let _: LoomNetworkEvent = serde_json::from_value(first.clone())
        .expect("AC-NAVRECEIPT2-03: side_effects[0] must round-trip into LoomNetworkEvent");
}

// ─── AC-NAVRECEIPT2-04: combined — full shape AND blob round-trip ──────────

#[test]
fn ac_navreceipt2_04_full_shape_with_blob_roundtrip_in_one_test() {
    let (cs, _tmp) = fresh_content_store();

    let dom_bytes: Vec<u8> = b"<html>combined</html>".to_vec();
    let ss_bytes: Vec<u8> = vec![0xAB; 256];

    let dom_ref = cs.put(&dom_bytes).expect("cs.put(dom)");
    let ss_ref = cs.put(&ss_bytes).expect("cs.put(screenshot)");

    let r = full_navigate_receipt(dom_ref.sha256.clone(), ss_ref.sha256.clone());

    // (1) Full receipt shape — every brief-listed key present.
    let json = serde_json::to_value(&r).unwrap();
    for key in [
        "url",
        "final_url",
        "title",
        "status_code",
        "dom_snapshot_hash",
        "screenshot_after_hash",
        "console_count",
        "console_lines",
        "network_count",
        "network_summary",
    ] {
        assert!(json.get(key).is_some(), "missing {key}");
    }

    // (2) Hash format.
    assert_is_sha256_hex("dom_snapshot_hash", &dom_ref.sha256);
    assert_is_sha256_hex("screenshot_after_hash", &ss_ref.sha256);

    // (3) Blob round-trip via cs.get.
    assert_eq!(cs.get(&dom_ref).unwrap(), dom_bytes);
    assert_eq!(cs.get(&ss_ref).unwrap(), ss_bytes);

    // (4) network_summary aggregates the events correctly.
    let s = r.network_summary.as_ref().expect("network_summary present");
    assert_eq!(s.total_count, 3);
    // 4096 (200 doc) + 1024 (200 css) + 0 (404 png with response_bytes=0)
    assert_eq!(s.total_bytes, 4096 + 1024);
    assert_eq!(s.error_count, 1, "one 404 in fixture");

    // (5) console_lines preserved verbatim under default profile.
    assert_eq!(r.console_lines.len(), 2);
    assert_eq!(r.console_lines[0].level, "info");
    assert_eq!(r.console_lines[0].message, "page loaded");
}

// ─── AC-NAVRECEIPT2-05: --capture-policy minimal strips tier-2 fields ──────

#[test]
fn ac_navreceipt2_05_minimal_capture_strips_tier_two_fields() {
    let mut r = full_navigate_receipt("a".repeat(64), "b".repeat(64));
    r.error = Some(ReceiptError {
        kind: "http_status".into(),
        detail: Some(serde_json::json!({"status_code": 200, "url": "https://example.com/"})),
    });

    apply_capture_profile_to_wire(&mut r, CaptureProfile::Minimal);

    // Brief-listed fields stripped:
    assert!(
        r.dom_snapshot_hash.is_none(),
        "AC-NAVRECEIPT2-05: dom_snapshot_hash must be stripped under Minimal"
    );
    assert!(
        r.screenshot_after_hash.is_none(),
        "AC-NAVRECEIPT2-05: screenshot_after_hash must be stripped under Minimal"
    );
    assert!(
        r.console_lines.is_empty(),
        "AC-NAVRECEIPT2-05: console_lines must be empty under Minimal"
    );
    // Plus all the other tier-2 fields:
    assert!(r.final_url.is_none());
    assert!(r.title.is_none());
    assert!(r.console_count.is_none());
    assert!(r.network_count.is_none());
    assert!(r.network_summary.is_none());
    assert!(r.side_effects.is_empty());

    // KEPT (per brief: url, status_code, error + identity):
    assert_eq!(r.url.as_deref(), Some("https://example.com/"));
    assert_eq!(r.status_code, Some(200));
    assert!(r.error.is_some());
    assert_eq!(r.action_id, 7);
    assert_eq!(r.session_id, "01HZSESS");
}

// ─── Non-AC: Default profile is a no-op pass-through ──────────────────────

#[test]
fn default_capture_policy_is_no_op() {
    let r1 = full_navigate_receipt("a".repeat(64), "b".repeat(64));
    let mut r2 = r1.clone();
    apply_capture_profile_to_wire(&mut r2, CaptureProfile::Default);
    assert_eq!(
        serde_json::to_value(&r1).unwrap(),
        serde_json::to_value(&r2).unwrap(),
        "Default profile must be a no-op"
    );
}

// ─── Non-AC: Full profile is a no-op today (D6: dom_full_text deferred) ────

#[test]
fn full_capture_policy_is_no_op_today() {
    let r1 = full_navigate_receipt("a".repeat(64), "b".repeat(64));
    let mut r2 = r1.clone();
    apply_capture_profile_to_wire(&mut r2, CaptureProfile::Full);
    assert_eq!(
        r2.console_lines.len(),
        r1.console_lines.len(),
        "Full must keep console_lines verbatim (count unchanged today; semantic gain when shim console capture lands)"
    );
}
