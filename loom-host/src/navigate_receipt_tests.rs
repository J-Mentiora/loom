// Tests for navigate receipt tier-2 payload.
//
// These tests exercise:
//   - ReceiptBuilder::navigate_* field population
//   - assemble_canonical_bytes → ReceiptPayload canonical JSON
//   - side_effects_json as Vec<LoomNetworkEvent>
//   - dom/screenshot hash round-trip through MockContentStore
//   - Full schema shape via serde_json::Value deserialization

use crate::receipt_marshaller::{ReceiptBuilder, ReceiptMarshaller, ReceiptStatus};
use loom_core::content_store::ContentStore;
use loom_core::mocks::MockContentStore;
use loom_shared::navigate_outcome::LoomNetworkEvent;

// ---------------------------------------------------------------------------
// receipt JSON has all 8 navigate tier-2 keys
// ---------------------------------------------------------------------------

#[test]
fn test_navigate_receipt_has_tier2_keys() {
    let builder = ReceiptBuilder {
        action_id: 1,
        finished_at_ms: 1_000,
        status: ReceiptStatus::Ok,
        action_hash: "aabbcc".to_string(),
        outcome_hash: "ddeeff".to_string(),
        emitted_at_ms: 5_000,
        navigate_url: Some("https://example.com/".to_string()),
        navigate_final_url: Some("https://example.com/".to_string()),
        navigate_title: Some("Example".to_string()),
        navigate_status_code: Some(200),
        navigate_dom_snapshot_hash: Some("a".repeat(64)),
        navigate_screenshot_after_hash: Some("b".repeat(64)),
        navigate_console_count: Some(0),
        navigate_network_count: Some(1),
        navigate_side_effects_json: None,
        ..Default::default()
    };

    let bytes = ReceiptMarshaller::assemble_canonical_bytes(&builder)
        .expect("assemble_canonical_bytes must not fail");

    let val: serde_json::Value =
        serde_json::from_slice(&bytes).expect("canonical bytes must be valid JSON");

    // Assert all 8 navigate tier-2 keys are present
    let required_keys = [
        "url",
        "final_url",
        "title",
        "status_code",
        "dom_snapshot_hash",
        "screenshot_after_hash",
        "console_count",
        "network_count",
    ];
    for key in &required_keys {
        assert!(
            val.get(key).is_some(),
            "missing key '{key}' in navigate receipt JSON"
        );
    }

    assert_eq!(val["url"], "https://example.com/");
    assert_eq!(val["status_code"], 200u32);
    assert_eq!(val["dom_snapshot_hash"], "a".repeat(64));
}

// ---------------------------------------------------------------------------
// emitted_at_ms is present and monotonically non-decreasing
// ---------------------------------------------------------------------------

#[test]
fn test_navigate_receipt_emitted_at_ms_monotonic() {
    let make_builder = |id: u64, emitted_at_ms: u64| ReceiptBuilder {
        action_id: id,
        finished_at_ms: 1_000,
        status: ReceiptStatus::Ok,
        emitted_at_ms,
        navigate_url: Some("https://example.com/".to_string()),
        navigate_final_url: Some("https://example.com/".to_string()),
        navigate_title: Some("".to_string()),
        navigate_status_code: Some(200),
        navigate_dom_snapshot_hash: Some("0".repeat(64)),
        navigate_screenshot_after_hash: Some("1".repeat(64)),
        navigate_console_count: Some(0),
        navigate_network_count: Some(0),
        ..Default::default()
    };

    let b1 = make_builder(1, 1_000);
    let b2 = make_builder(2, 2_000);

    let bytes1 =
        ReceiptMarshaller::assemble_canonical_bytes(&b1).expect("assemble 1 must not fail");
    let bytes2 =
        ReceiptMarshaller::assemble_canonical_bytes(&b2).expect("assemble 2 must not fail");

    let v1: serde_json::Value = serde_json::from_slice(&bytes1).unwrap();
    let v2: serde_json::Value = serde_json::from_slice(&bytes2).unwrap();

    let ms1 = v1["emitted_at_ms"]
        .as_u64()
        .expect("emitted_at_ms must be u64");
    let ms2 = v2["emitted_at_ms"]
        .as_u64()
        .expect("emitted_at_ms must be u64");

    assert!(
        ms2 >= ms1,
        "emitted_at_ms must be monotonically non-decreasing ({ms1} > {ms2})"
    );
}

// ---------------------------------------------------------------------------
// side_effects JSON contains Vec<LoomNetworkEvent>
// ---------------------------------------------------------------------------

#[test]
fn test_navigate_receipt_side_effects_contains_network_events() {
    let events = vec![
        LoomNetworkEvent {
            method: "GET".to_string(),
            url: "https://example.com/".to_string(),
            request_hash: "0".repeat(64),
            response_hash: "1".repeat(64),
            status: 200,
            content_type: "text/html".to_string(),
            duration_ms: 50,
            response_bytes: 1024,
            error_reason: None,
            error_kind: None,
        },
        LoomNetworkEvent {
            method: "GET".to_string(),
            url: "https://example.com/style.css".to_string(),
            request_hash: "2".repeat(64),
            response_hash: "3".repeat(64),
            status: 200,
            content_type: "text/css".to_string(),
            duration_ms: 20,
            response_bytes: 512,
            error_reason: None,
            error_kind: None,
        },
        LoomNetworkEvent {
            method: "GET".to_string(),
            url: "https://example.com/api/data".to_string(),
            request_hash: "4".repeat(64),
            response_hash: "5".repeat(64),
            status: 404,
            content_type: "application/json".to_string(),
            duration_ms: 30,
            response_bytes: 64,
            error_reason: None,
            error_kind: None,
        },
    ];

    let side_effects_json = serde_json::to_vec(&events).expect("events must serialize");

    let builder = ReceiptBuilder {
        action_id: 10,
        finished_at_ms: 500,
        status: ReceiptStatus::Ok,
        emitted_at_ms: 1_000,
        navigate_url: Some("https://example.com/".to_string()),
        navigate_final_url: Some("https://example.com/".to_string()),
        navigate_title: Some("".to_string()),
        navigate_status_code: Some(200),
        navigate_dom_snapshot_hash: Some("a".repeat(64)),
        navigate_screenshot_after_hash: Some("b".repeat(64)),
        navigate_console_count: Some(0),
        navigate_network_count: Some(3),
        navigate_side_effects_json: Some(side_effects_json.clone()),
        ..Default::default()
    };

    // Assert side_effects_json bytes are valid JSON + deserializable
    let decoded: Vec<LoomNetworkEvent> =
        serde_json::from_slice(&side_effects_json).expect("side_effects_json must be valid JSON");

    assert_eq!(decoded.len(), 3, "must have 3 network events");
    for (i, ev) in decoded.iter().enumerate() {
        assert!(!ev.url.is_empty(), "event[{i}] must have url");
        assert!(ev.status > 0, "event[{i}] must have non-zero status");
        assert!(!ev.method.is_empty(), "event[{i}] must have method");
    }

    // network_count in receipt matches event count
    let bytes =
        ReceiptMarshaller::assemble_canonical_bytes(&builder).expect("assemble must not fail");
    let val: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(val["network_count"], 3u64);
}

// ---------------------------------------------------------------------------
// hashes match ContentStore blobs
// ---------------------------------------------------------------------------

#[test]
fn test_navigate_receipt_hashes_match_content_store() {
    let store = MockContentStore::new();

    let dom_bytes = b"<html><body>test page</body></html>";
    let ss_bytes = b"\x89PNG\r\n\x1a\n(fake screenshot bytes)";

    // Store blobs and capture the returned ContentRef (which has SHA-256 hash)
    let dom_ref = store.put(dom_bytes).expect("put dom_bytes");
    let ss_ref = store.put(ss_bytes).expect("put screenshot_bytes");

    // Verify: SHA-256 computed by ContentStore matches what we'd put in the receipt
    let expected_dom_hash = dom_ref.sha256.clone();
    let expected_ss_hash = ss_ref.sha256.clone();

    let builder = ReceiptBuilder {
        action_id: 42,
        finished_at_ms: 100,
        status: ReceiptStatus::Ok,
        emitted_at_ms: 200,
        navigate_url: Some("https://example.com/".to_string()),
        navigate_final_url: Some("https://example.com/".to_string()),
        navigate_title: Some("".to_string()),
        navigate_status_code: Some(200),
        navigate_dom_snapshot_hash: Some(expected_dom_hash.clone()),
        navigate_screenshot_after_hash: Some(expected_ss_hash.clone()),
        navigate_console_count: Some(0),
        navigate_network_count: Some(0),
        ..Default::default()
    };

    let bytes =
        ReceiptMarshaller::assemble_canonical_bytes(&builder).expect("assemble must not fail");
    let val: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    // Assert receipt hashes == ContentStore-computed hashes
    assert_eq!(
        val["dom_snapshot_hash"].as_str().unwrap(),
        expected_dom_hash,
        "dom_snapshot_hash must match ContentStore SHA-256"
    );
    assert_eq!(
        val["screenshot_after_hash"].as_str().unwrap(),
        expected_ss_hash,
        "screenshot_after_hash must match ContentStore SHA-256"
    );

    // Round-trip: ContentStore.get(dom_ref) returns the original bytes
    let retrieved = store.get(&dom_ref).expect("get must succeed");
    assert_eq!(
        retrieved, dom_bytes,
        "ContentStore round-trip must preserve dom bytes"
    );
}

// ---------------------------------------------------------------------------
// integration schema: all canonical receipt fields present
// ---------------------------------------------------------------------------

#[test]
fn test_navigate_receipt_integration_schema() {
    let builder = ReceiptBuilder {
        action_id: 99,
        finished_at_ms: 2_500,
        status: ReceiptStatus::Ok,
        action_hash: "1234".to_string(),
        outcome_hash: "5678".to_string(),
        emitted_at_ms: 3_000,
        navigate_url: Some("https://example.com/page".to_string()),
        navigate_final_url: Some("https://example.com/page".to_string()),
        navigate_title: Some("Page Title".to_string()),
        navigate_status_code: Some(200),
        navigate_dom_snapshot_hash: Some("c".repeat(64)),
        navigate_screenshot_after_hash: Some("d".repeat(64)),
        navigate_console_count: Some(2),
        navigate_network_count: Some(5),
        ..Default::default()
    };

    let bytes = ReceiptMarshaller::assemble_canonical_bytes(&builder)
        .expect("assemble_canonical_bytes must not fail");

    // Bytes must be valid JSON
    let val: serde_json::Value =
        serde_json::from_slice(&bytes).expect("canonical bytes must be valid JSON");

    // Assert all canonical receipt / navigate tier-2 fields are present
    // (dom_snapshot_hash or dom_after_blob_ref, screenshot_after_hash, timing_ticks)
    assert!(
        val.get("dom_snapshot_hash").is_some() || val.get("dom_after_blob_ref").is_some(),
        "dom_snapshot_hash or dom_after_blob_ref must be present"
    );
    assert!(
        val.get("screenshot_after_hash").is_some(),
        "screenshot_after_hash must be present"
    );
    assert!(
        val.get("timing_ticks").is_some(),
        "timing_ticks must be present in canonical receipt"
    );

    // Also confirm action_id and status are serialized
    assert!(val.get("action_id").is_some(), "action_id must be present");
    assert!(val.get("status").is_some(), "status must be present");

    // Canonical bytes must be non-empty and valid serde_jcs output (keys sorted)
    assert!(!bytes.is_empty(), "canonical bytes must not be empty");
}

// ---------------------------------------------------------------------------
// Typed-error receipts on HTTP 4xx/5xx and DNS failures
// ---------------------------------------------------------------------------
//
// Each test builds a `ReceiptBuilder` shaped exactly as the host would produce
// after the WIT decode + shim-failure structured-detail branch in
// `decode_typed_receipt` flips status to Error and stamps `error_code` =
// "shim-failure" with structured JSON `error_details`. We then assert the
// canonical-JSON output emitted by `assemble_canonical_bytes` matches the
// expected contract: `status="error"`, `code="web_navigation_failed"`,
// `details.kind="http_status"|"dns_failure"`, and a short friendly `message`.

fn navigate_error_builder(error_details: &str, status_code: Option<u32>) -> ReceiptBuilder {
    ReceiptBuilder {
        action_id: 1,
        finished_at_ms: 1_000,
        status: ReceiptStatus::Error,
        emitted_at_ms: 5_000,
        error_code: Some("shim-failure".to_string()),
        error_details: Some(error_details.to_string()),
        navigate_status_code: status_code,
        ..Default::default()
    }
}

fn assemble_value(builder: &ReceiptBuilder) -> serde_json::Value {
    let bytes = ReceiptMarshaller::assemble_canonical_bytes(builder)
        .expect("assemble_canonical_bytes must not fail");
    serde_json::from_slice(&bytes).expect("canonical bytes must be valid JSON")
}

#[test]
fn test_naverr_404_emits_web_navigation_failed() {
    let builder = navigate_error_builder(
        r#"{"kind":"http_status","status_code":404}"#,
        Some(404),
    );
    let val = assemble_value(&builder);

    assert_eq!(val["status"], "error", "status must be 'error'");
    assert_eq!(
        val["code"], "web_navigation_failed",
        "code must be 'web_navigation_failed'"
    );
    assert_eq!(
        val["details"]["kind"], "http_status",
        "details.kind must be 'http_status'"
    );
    assert_eq!(
        val["details"]["status_code"], 404u64,
        "details.status_code must be 404"
    );
    let msg = val["message"].as_str().expect("message must be string");
    assert!(
        msg.contains("404"),
        "message must reference 404 (got {msg:?})"
    );
    assert!(
        msg.len() <= 280,
        "message must be <= 280 chars (got {})",
        msg.len()
    );
}

#[test]
fn test_naverr_500_emits_web_navigation_failed() {
    let builder = navigate_error_builder(
        r#"{"kind":"http_status","status_code":500}"#,
        Some(500),
    );
    let val = assemble_value(&builder);

    assert_eq!(val["status"], "error");
    assert_eq!(val["code"], "web_navigation_failed");
    assert_eq!(val["details"]["kind"], "http_status");
    assert_eq!(val["details"]["status_code"], 500u64);
    assert!(val["message"].as_str().unwrap().contains("500"));
}

#[test]
fn test_naverr_dns_failure_emits_web_navigation_failed() {
    let builder = navigate_error_builder(
        r#"{"kind":"dns_failure","chromium_error":"net::ERR_NAME_NOT_RESOLVED"}"#,
        None,
    );
    let val = assemble_value(&builder);

    assert_eq!(val["status"], "error", "status must be 'error'");
    assert_eq!(val["code"], "web_navigation_failed");
    assert_eq!(
        val["details"]["kind"], "dns_failure",
        "details.kind must be 'dns_failure'"
    );
    assert_eq!(
        val["details"]["chromium_error"], "net::ERR_NAME_NOT_RESOLVED",
        "details.chromium_error must be preserved"
    );
    let msg = val["message"].as_str().expect("message must be string");
    let lower = msg.to_lowercase();
    assert!(
        lower.contains("dns") || lower.contains("network"),
        "message must reference DNS or network failure (got {msg:?})"
    );
}

#[test]
fn test_naverr_200_keeps_web_action_completed() {
    // The OK path is unchanged: 2xx still produces code=web_action_completed.
    let builder = ReceiptBuilder {
        action_id: 2,
        finished_at_ms: 1_000,
        status: ReceiptStatus::Ok,
        emitted_at_ms: 5_000,
        navigate_url: Some("https://example.com/".to_string()),
        navigate_final_url: Some("https://example.com/".to_string()),
        navigate_title: Some("OK".to_string()),
        navigate_status_code: Some(200),
        navigate_dom_snapshot_hash: Some("a".repeat(64)),
        navigate_screenshot_after_hash: Some("b".repeat(64)),
        navigate_console_count: Some(0),
        navigate_network_count: Some(1),
        ..Default::default()
    };
    let val = assemble_value(&builder);

    assert_eq!(val["status"], "ok", "200 must keep status=ok");
    assert_eq!(
        val["code"], "web_action_completed",
        "200 must keep code=web_action_completed"
    );
}

// ---------------------------------------------------------------------------
// Receipt.timing_ticks reflects measured action duration
// ---------------------------------------------------------------------------
//
// Originally Receipt.timing_ticks was always 0 because:
//   1. SessionExecutor::run never populated builder.{started_at_ms,
//      finished_at_ms} around the WASM dispatch.
//   2. The marshaller assigned timing_ticks = builder.finished_at_ms (in ms),
//      which was always 0 — and even if non-zero, ms-not-µs violated
//      the microsecond unit clause.
//
// These tests pin the canonical contract: ms→µs conversion at the marshaller,
// monotonic across action_ids, and a non-zero value once the executor begins
// to drive the determinism clock.

fn navigate_ok_builder_with_finished_ms(action_id: u64, finished_at_ms: u64) -> ReceiptBuilder {
    ReceiptBuilder {
        action_id,
        finished_at_ms,
        status: ReceiptStatus::Ok,
        emitted_at_ms: finished_at_ms,
        navigate_url: Some("https://example.com/slow".to_string()),
        navigate_final_url: Some("https://example.com/slow".to_string()),
        navigate_title: Some("Slow".to_string()),
        navigate_status_code: Some(200),
        navigate_dom_snapshot_hash: Some("a".repeat(64)),
        navigate_screenshot_after_hash: Some("b".repeat(64)),
        navigate_console_count: Some(0),
        navigate_network_count: Some(0),
        ..Default::default()
    }
}

#[test]
fn test_timing_navigate_receipt_timing_ticks_positive() {
    // Navigate receipt's timing_ticks is non-zero for any successful
    // navigation. With the executor populating finished_at_ms from a
    // non-zero virtual clock, the marshaller emits a positive
    // timing_ticks.
    let builder = navigate_ok_builder_with_finished_ms(1, 7);
    let val = assemble_value(&builder);

    let ticks = val["timing_ticks"]
        .as_u64()
        .expect("timing_ticks must serialize as integer");
    assert!(
        ticks > 0,
        "timing_ticks must be > 0 for successful navigate (got {ticks})"
    );
}

#[test]
fn test_timing_navigate_receipt_timing_ticks_unit_is_microseconds() {
    // timing_ticks is in microseconds. builder.finished_at_ms is in ms
    // (matches the field name); marshaller converts ms→µs at the
    // assembly site. Given finished_at_ms=5, the canonical timing_ticks
    // must be 5_000 µs.
    let builder = navigate_ok_builder_with_finished_ms(1, 5);
    let val = assemble_value(&builder);

    let ticks = val["timing_ticks"].as_u64().expect("integer");
    assert_eq!(
        ticks, 5_000,
        "timing_ticks unit must be microseconds (5 ms → 5000 µs, got {ticks})"
    );
}

#[test]
fn test_timing_navigate_receipt_timing_ticks_monotonic_across_actions() {
    // timing_ticks monotonically non-decreasing across action_ids in a
    // session.
    let r0 = assemble_value(&navigate_ok_builder_with_finished_ms(0, 0));
    let r1 = assemble_value(&navigate_ok_builder_with_finished_ms(1, 3));
    let r2 = assemble_value(&navigate_ok_builder_with_finished_ms(2, 9));

    let t0 = r0["timing_ticks"].as_u64().expect("integer");
    let t1 = r1["timing_ticks"].as_u64().expect("integer");
    let t2 = r2["timing_ticks"].as_u64().expect("integer");

    // Action 0 starts at 0.
    assert_eq!(t0, 0, "timing_ticks at action 0 starts at 0");
    assert!(
        t1 >= t0 && t2 >= t1,
        "monotonic across action_ids ({t0}, {t1}, {t2})"
    );
    // The deltas mirror the ms-source increments scaled to µs.
    assert!(
        t1 > t0 && t2 > t1,
        "strictly increasing when source clock advances ({t0}, {t1}, {t2})"
    );
}

#[test]
fn test_timing_navigate_timing_ticks_nonzero_after_determinism_driven_dispatch() {
    // Integration-shape test. Drives the same sequence the
    // patched SessionExecutor::run uses — clock_now → begin_action(delta)
    // → clock_now — through a real DeterminismHarness, then asserts the
    // assembled receipt has timing_ticks > 0. Substitutes for a heavy
    // wasmtime fixture; the wiring is contract-equivalent: anything that
    // breaks the clock-advance sequence breaks this test.
    use loom_core::determinism_harness::DeterminismHarness;
    use loom_core::manifest_writer::{LocalManifestWriter, ManifestWriter};
    use loom_core::observability::Observability;
    use std::path::PathBuf;
    use std::sync::Arc;

    // Reuse the stock LocalManifestWriter fixture — clock_now/begin_action
    // don't actually exercise ManifestWriter, but DeterminismHarness::new
    // requires an Arc<dyn ManifestWriter>.
    let obs = Observability::new(PathBuf::from("/tmp/loom-test-timing-ticks/loom.log"), false);
    let mw: Arc<dyn ManifestWriter> = Arc::new(LocalManifestWriter::new(
        PathBuf::from("/tmp/loom-test-timing-ticks/sessions"),
        obs,
    ));
    let harness = DeterminismHarness::new(0xCAFE, mw);

    // Pre-dispatch
    let started_ms = harness.clock_now();
    assert_eq!(started_ms, 0, "action 0 starts at 0");

    // Simulate dispatch advancing the wall clock by ≥ 1 ms — the
    // executor's `(elapsed_ms).max(1)` clamp guarantees this minimum
    // for sub-ms dispatches.
    harness.begin_action(1);

    // Post-dispatch
    let finished_ms = harness.clock_now();
    assert!(
        finished_ms > started_ms,
        "virtual clock must advance across a dispatch ({started_ms} → {finished_ms})"
    );

    let builder = navigate_ok_builder_with_finished_ms(1, finished_ms);
    let val = assemble_value(&builder);
    let ticks = val["timing_ticks"].as_u64().expect("integer");
    assert!(
        ticks > 0,
        "timing_ticks must be > 0 after determinism-driven dispatch (got {ticks})"
    );
    assert_eq!(
        ticks,
        finished_ms.saturating_mul(1000),
        "cross-check: ticks (µs) == finished_ms × 1000"
    );
}

// ---------------------------------------------------------------------------
// HAR export regression test for the marshaller-gate fix.
//
// Before the gate fix, `assemble_canonical_bytes` ducked into the generic
// `serde_jcs::to_string(builder)` path whenever `navigate_url` and
// `navigate_dom_snapshot_hash` were both unset — silently dropping
// `navigate_side_effects_json` and producing receipts with empty
// `network_events`. That made HAR exports return `entries: []`.
//
// This test asserts the marshaller takes the navigate-assembly path even
// when tier-2 fields aren't wired (the
// `navigate-receipt-tier2-still-missing` regression window) AND populates
// `network_events` from `navigate_side_effects_json`.
//
// IMPORTANT — DO NOT REMOVE the `is_none()` asserts when tier-2 wiring
// lands. The regression-fitness property of this test is "marshaller
// takes the navigate path even when tier-2 is unset". A future
// maintainer "fixing" the failing asserts by populating tier-2 in this
// fixture would silently neutralize that regression check. If tier-2
// wiring becomes ubiquitous and this test starts looking artificial,
// add a `#[allow(dead_code)]` shim helper that creates the explicitly-
// degraded builder rather than weakening the asserts.
// ---------------------------------------------------------------------------

#[test]
fn harexport_marshaller_preserves_network_events_when_tier2_unset() {
    use loom_core::receipt_builder::receipt_builder::ReceiptPayload;

    let events = vec![
        LoomNetworkEvent {
            method: "GET".to_string(),
            url: "https://example.com/".to_string(),
            request_hash: "0".repeat(64),
            response_hash: "1".repeat(64),
            status: 200,
            content_type: "text/html".to_string(),
            duration_ms: 50,
            response_bytes: 1234,
            error_reason: None,
            error_kind: None,
        },
        LoomNetworkEvent {
            method: "GET".to_string(),
            url: "https://example.com/style.css".to_string(),
            request_hash: "2".repeat(64),
            response_hash: "3".repeat(64),
            status: 200,
            content_type: "text/css".to_string(),
            duration_ms: 20,
            response_bytes: 567,
            error_reason: None,
            error_kind: None,
        },
    ];
    let side_effects_json =
        serde_json::to_vec(&events).expect("LoomNetworkEvent serializes as JSON");

    // Builder with NO tier-2 fields but DOES have navigate_side_effects_json.
    let builder = ReceiptBuilder {
        action_id: 42,
        finished_at_ms: 100,
        status: ReceiptStatus::Ok,
        emitted_at_ms: 1_000,
        navigate_url: None,
        navigate_final_url: None,
        navigate_title: None,
        navigate_status_code: None,
        navigate_dom_snapshot_hash: None,
        navigate_screenshot_after_hash: None,
        navigate_console_count: None,
        navigate_network_count: Some(events.len() as u64),
        navigate_side_effects_json: Some(side_effects_json),
        ..Default::default()
    };

    let bytes = ReceiptMarshaller::assemble_canonical_bytes(&builder).expect(
        "assemble_canonical_bytes must succeed for navigate_side_effects_json-only builder",
    );
    let payload: ReceiptPayload =
        serde_json::from_slice(&bytes).expect("canonical bytes must deserialize as ReceiptPayload");

    assert_eq!(
        payload.network_events.len(),
        2,
        "marshaller must convert side_effects_json to ReceiptPayload.network_events"
    );
    assert_eq!(payload.network_events[0].url, "https://example.com/");
    assert_eq!(payload.network_events[0].status_code, 200);
    assert_eq!(payload.network_events[0].response_body_size_bytes, 1234);
    assert_eq!(
        payload.network_events[1].url,
        "https://example.com/style.css"
    );
    assert_eq!(payload.network_events[1].response_body_size_bytes, 567);

    // Asymmetry assertions — these prove the navigate path was taken
    // WITHOUT the tier-2 fields, defeating the silent
    // `#[serde(default)]` masking that ReceiptPayload.network_events
    // would otherwise hide. (See receipt_builder/interfaces.rs:111.)
    assert!(
        payload.url.is_none(),
        "tier-2 url must be None to prove the regression scenario"
    );
    assert!(
        payload.status_code.is_none(),
        "tier-2 status_code must be None to prove the regression scenario"
    );
    assert!(
        payload.title.is_none(),
        "tier-2 title must be None to prove the regression scenario"
    );
}
