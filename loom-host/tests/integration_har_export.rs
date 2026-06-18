//! HAR export end-to-end integration test.
//!
//! Exercises the loom-host marshaller + loom-core WAL-writer + loom-core
//! exporter together for a 2-navigate session: one `WebActionCompleted`
//! (status 200) + one `WebNavigationFailed` (status 404). Both receipts
//! must produce HAR entries with correct request.url + response.status,
//! and the resulting HAR must validate against the vendored HAR 1.2 JSON
//! Schema fixture in `loom-core/tests/fixtures/har-1.2.schema.json`.
//!
//! This test asserts the integration of the changes in this PR:
//!
//!   1. P0 (host_impl + session_executor): typed-error receipts now
//!      carry `navigate_side_effects_json` populated from the captured
//!      shim events. The 404 builder in this test models exactly that
//!      post-decode-typed-receipt state.
//!
//!   2. P1 (marshaller gate): `assemble_canonical_bytes` takes the
//!      navigate path even when tier-2 fields aren't fully wired,
//!      converting `LoomNetworkEvent` → `NetworkEvent` so HAR sees
//!      per-event url/status. Both builders go through the marshaller.
//!
//!   3. The exporter is unchanged (already correct); the test confirms
//!      it produces a HAR-1.2-valid document.
//!
//! Why no fake-chromium / WIT runtime: the host_impl + session_executor
//! transforms are covered end-to-end by the unit tests
//! `typed_error_preserves_network_events` and
//! `harexport_marshaller_preserves_network_events_when_tier2_unset`.
//! Adding a fake-chromium-driven WIT-runtime path would require
//! building the loom-surface-web WASM artifact at test time, which is
//! out of scope for this PR. This test exercises the marshaller →
//! WAL-writer → exporter pipeline that those unit tests don't reach.
//! The HAR export contract is satisfied: a 2-receipt session round-trips
//! through the export path and yields HAR entries matching navigated
//! URLs + statuses, with full schema validation.
//!
//! Run via:
//!   `cargo test -p loom-host --test integration_har_export`

use loom_core::manifest_writer::ManifestEntry;
use loom_host::receipt_marshaller::{ReceiptBuilder, ReceiptMarshaller, ReceiptStatus};
use loom_shared::navigate_outcome::LoomNetworkEvent;
use std::fs;
use std::sync::Arc;
use tempfile::TempDir;

/// Build the success navigate ReceiptBuilder for `http://fake.test/status/200`.
fn success_builder() -> ReceiptBuilder {
    let event = LoomNetworkEvent {
        method: "GET".into(),
        url: "http://fake.test/status/200".into(),
        request_hash: "0".repeat(64),
        response_hash: "1".repeat(64),
        status: 200,
        content_type: "text/html".into(),
        duration_ms: 12,
        response_bytes: 1024,
        error_reason: None,
        error_kind: None,
    };
    let side_effects_json = serde_json::to_vec(&[event]).expect("serialize events");

    ReceiptBuilder {
        action_id: 1,
        finished_at_ms: 100,
        status: ReceiptStatus::Ok,
        emitted_at_ms: 1_000,
        navigate_url: Some("http://fake.test/status/200".into()),
        navigate_final_url: Some("http://fake.test/status/200".into()),
        navigate_title: Some("ok".into()),
        navigate_status_code: Some(200),
        navigate_dom_snapshot_hash: Some("a".repeat(64)),
        navigate_screenshot_after_hash: Some("b".repeat(64)),
        navigate_console_count: Some(0),
        navigate_network_count: Some(1),
        navigate_side_effects_json: Some(side_effects_json),
        ..Default::default()
    }
}

/// Build the typed-error navigate ReceiptBuilder for
/// `http://fake.test/status/404`. Models the post-`decode_typed_receipt`
/// state after P0: error_details is the cleaned JSON (no `_network_events`),
/// navigate_side_effects_json is populated, status flipped to Error.
fn error_builder_for_404() -> ReceiptBuilder {
    let event = LoomNetworkEvent {
        method: "GET".into(),
        url: "http://fake.test/status/404".into(),
        request_hash: "2".repeat(64),
        response_hash: "3".repeat(64),
        status: 404,
        content_type: "text/plain".into(),
        duration_ms: 8,
        response_bytes: 22,
        error_reason: None,
        error_kind: None,
    };
    let side_effects_json = serde_json::to_vec(&[event]).expect("serialize events");

    // Mirror the cleaned shape session_executor::decode_typed_receipt
    // produces after P0 strips `_network_events`.
    let cleaned_detail = serde_json::json!({
        "kind": "http_status",
        "url": "http://fake.test/status/404",
        "status_code": 404,
    })
    .to_string();

    ReceiptBuilder {
        action_id: 2,
        finished_at_ms: 200,
        status: ReceiptStatus::Error,
        error_code: Some("shim-failure".into()),
        error_details: Some(cleaned_detail),
        emitted_at_ms: 2_000,
        // Tier-2 fields stay None on the typed-error path (status_code is
        // lifted from detail.status_code by session_executor).
        navigate_url: None,
        navigate_final_url: None,
        navigate_title: None,
        navigate_status_code: Some(404),
        navigate_dom_snapshot_hash: None,
        navigate_screenshot_after_hash: None,
        navigate_console_count: None,
        navigate_network_count: Some(1),
        navigate_side_effects_json: Some(side_effects_json),
        ..Default::default()
    }
}

/// Write a minimal manifest.wal with a Header + N ActionReceipt entries.
fn write_wal(sessions_root: &std::path::Path, session_id: &str, receipts: &[Vec<u8>]) {
    let session_dir = sessions_root.join(session_id);
    fs::create_dir_all(&session_dir).expect("create session dir");

    let header = ManifestEntry::Header {
        session_id: session_id.to_string(),
        started_at_ms: 0,
        prev_hash: None,
        budgets: None,
        capture_policy: None,
        seed: None,
        determinism_enabled: None,
    };
    let mut lines = vec![serde_json::to_string(&header).expect("serialize header")];
    for (i, bytes) in receipts.iter().enumerate() {
        let entry = ManifestEntry::ActionReceipt {
            action_id: (i + 1) as u64,
            emitted_at_ms: (i + 1) as u64 * 1_000,
            receipt_canonical_bytes: bytes.clone(),
            prev_hash: "0".repeat(64),
        };
        lines.push(serde_json::to_string(&entry).expect("serialize entry"));
    }
    fs::write(session_dir.join("manifest.wal"), lines.join("\n")).expect("write WAL");
}

fn load_har_schema() -> serde_json::Value {
    // Co-located with the loom-core schema-validation tests so a single
    // fixture is the source of truth.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("loom-core")
        .join("tests")
        .join("fixtures")
        .join("har-1.2.schema.json");
    let bytes = fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

#[test]
fn harexport_05_two_navigate_session_har_contents() {
    use loom_core::content_store::{ContentStore, LocalContentStore};
    use loom_core::exporters::Exporter;
    use loom_core::observability::Observability;

    let tmp = TempDir::new().expect("tempdir");
    let sessions_root = tmp.path().join("sessions");
    fs::create_dir_all(&sessions_root).expect("create sessions root");

    // Marshal both receipts via the REAL loom-host marshaller — this
    // exercises P1's gate fix (and the navigate path's
    // LoomNetworkEvent → NetworkEvent conversion) for both the
    // success path and the typed-error path.
    let success_bytes = ReceiptMarshaller::assemble_canonical_bytes(&success_builder())
        .expect("marshal success builder");
    let error_bytes = ReceiptMarshaller::assemble_canonical_bytes(&error_builder_for_404())
        .expect("marshal error builder");

    write_wal(
        &sessions_root,
        "01HZHAREXP05",
        &[success_bytes, error_bytes],
    );

    let store_root = tmp.path().join("store");
    fs::create_dir_all(&store_root).expect("store root");
    let obs = Observability::new(store_root.join("loom.log"), false);
    let cs: Arc<dyn ContentStore> = Arc::new(LocalContentStore::new(store_root, obs));

    let exporter = Exporter::new(sessions_root, cs);
    let bytes = exporter.export_har("01HZHAREXP05").expect("export_har");
    let har: serde_json::Value = serde_json::from_slice(&bytes).expect("HAR is valid JSON");

    let entries = har["log"]["entries"]
        .as_array()
        .expect("log.entries is an array");
    // Each builder carries exactly one LoomNetworkEvent (the document
    // request); the marshaller emits one HAR entry per event. A future
    // sub-resource fan-out scenario should be a NEW test, not loosened
    // here.
    assert_eq!(
        entries.len(),
        2,
        "expected exactly 2 entries (one document request per navigate); got {}",
        entries.len()
    );

    // At least one entry per navigated URL, with response.status equal
    // to the actual HTTP status the shim captured. The 404 case is the
    // regression scenario that the typed-error fix enables.
    let entry_for = |url: &str, status: u64| -> bool {
        entries.iter().any(|e| {
            e["request"]["url"].as_str() == Some(url)
                && e["response"]["status"].as_u64() == Some(status)
        })
    };
    assert!(
        entry_for("http://fake.test/status/200", 200),
        "expected entry for /status/200 with status=200"
    );
    assert!(
        entry_for("http://fake.test/status/404", 404),
        "expected entry for /status/404 with status=404 — typed-error \
         navigates must propagate captured network_events to HAR"
    );

    // network-accounting-har: HAR must carry request.method + response
    // content.size per entry (not just url+status). Both builders go through
    // the marshaller with determinism_enabled defaulting false (direct
    // builders model a no-determinism export), so the captured size survives
    // into the canonical receipt the exporter reads.
    let entry_200 = entries
        .iter()
        .find(|e| e["request"]["url"].as_str() == Some("http://fake.test/status/200"))
        .expect("entry for /status/200");
    assert_eq!(
        entry_200["request"]["method"].as_str(),
        Some("GET"),
        "HAR request.method must be populated from the captured event"
    );
    assert_eq!(
        entry_200["response"]["content"]["size"].as_u64(),
        Some(1024),
        "HAR response.content.size must reflect response_body_size_bytes"
    );

    // HAR validates against the vendored Draft-04 schema.
    let schema = load_har_schema();
    let validator =
        jsonschema::validator_for(&schema).expect("vendored HAR 1.2 schema must compile");
    let errors: Vec<String> = validator
        .iter_errors(&har)
        .map(|e| format!("{e}"))
        .collect();
    assert!(
        errors.is_empty(),
        "HAR must validate against HAR 1.2 schema; got {} error(s):\n{}",
        errors.len(),
        errors.join("\n")
    );
}
