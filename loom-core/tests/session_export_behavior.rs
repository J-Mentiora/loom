//! Integration tests for the `session-export` feature.
//!
//! - JSON export has manifest and content_blob_index
//! - HAR export has one entry per ActionReceipt
//! - Tarball export contains manifest.json and blob entries
//! - HAR output validates against HAR 1.2 schema

use loom_core::content_store::{ContentRef, ContentStore, LocalContentStore};
use loom_core::exporters::Exporter;
use loom_core::manifest_writer::ManifestEntry;
use loom_core::observability::Observability;
use loom_core::receipt_builder::{NetworkEvent, ReceiptBuilder};
use std::fs;
use std::sync::Arc;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Write a minimal WAL: one Header + N ActionReceipt entries.
fn write_wal(sessions_root: &std::path::Path, session_id: &str, receipts: &[Vec<u8>]) {
    let session_dir = sessions_root.join(session_id);
    fs::create_dir_all(&session_dir).unwrap();

    let header = ManifestEntry::Header {
        session_id: session_id.to_string(),
        started_at_ms: 0,
        prev_hash: None,
        budgets: None,
        capture_policy: None,
        seed: None,
        determinism_enabled: None,
    };
    let mut lines = vec![serde_json::to_string(&header).unwrap()];

    for (i, receipt_bytes) in receipts.iter().enumerate() {
        let entry = ManifestEntry::ActionReceipt {
            action_id: (i + 1) as u64,
            emitted_at_ms: (i + 1) as u64 * 1000,
            receipt_canonical_bytes: receipt_bytes.clone(),
            prev_hash: "0".repeat(64),
        };
        lines.push(serde_json::to_string(&entry).unwrap());
    }

    fs::write(session_dir.join("manifest.wal"), lines.join("\n")).unwrap();
}

fn dummy_receipt() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"op": "click", "target": "#btn"})).unwrap()
}

/// Build a navigate-shaped ReceiptPayload's canonical bytes with embedded
/// `network_events`. HAR export reads these per-event to populate entries.
fn navigate_receipt_bytes(
    action_id: &str,
    url: &str,
    status_code: u32,
    network_events: Vec<NetworkEvent>,
) -> Vec<u8> {
    let dummy_ref = ContentRef {
        sha256: "0".repeat(64),
        size_bytes: 0,
    };
    let mut p = ReceiptBuilder::build_navigate_receipt(
        action_id.to_string(),
        0,
        dummy_ref.clone(),
        dummy_ref,
        network_events,
        Vec::new(),
    );
    p.url = Some(url.to_string());
    p.status_code = Some(status_code);
    p.canonical_bytes().unwrap()
}

/// Construct a NetworkEvent with the given URL, status, body size, and MIME.
fn net_event(method: &str, url: &str, status: u32, size: u64, mime: &str) -> NetworkEvent {
    NetworkEvent {
        method: method.to_string(),
        url: url.to_string(),
        status_code: status,
        response_body_sha256_hex: "0".repeat(64),
        response_body_size_bytes: size,
        response_body_ref: None,
        timing_ticks: 50_000, // 50 ms
        content_type: mime.to_string(),
    }
}

fn content_store_fixture(tmp: &TempDir) -> Arc<dyn ContentStore> {
    let store_root = tmp.path().join("store");
    fs::create_dir_all(&store_root).unwrap();
    let obs = Observability::new(store_root.join("loom.log"), false);
    Arc::new(LocalContentStore::new(store_root, obs))
}

// ---------------------------------------------------------------------------
// JSON export
// ---------------------------------------------------------------------------

#[test]
fn cli_03_1_json_export_has_manifest_and_blob_index() {
    let tmp = TempDir::new().unwrap();
    let sessions_root = tmp.path().join("sessions");
    fs::create_dir_all(&sessions_root).unwrap();
    let cs = content_store_fixture(&tmp);

    write_wal(
        &sessions_root,
        "01HZEXPORT001",
        &[dummy_receipt(), dummy_receipt(), dummy_receipt()],
    );

    let exporter = Exporter::new(sessions_root, cs);
    let bytes = exporter.export_json("01HZEXPORT001").unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert!(
        json.get("manifest").is_some(),
        "export_json must have 'manifest' key"
    );
    assert!(
        json.get("content_blob_index").is_some(),
        "export_json must have 'content_blob_index' key"
    );
}

#[test]
fn cli_03_1_json_export_is_valid_json() {
    let tmp = TempDir::new().unwrap();
    let sessions_root = tmp.path().join("sessions");
    fs::create_dir_all(&sessions_root).unwrap();
    let cs = content_store_fixture(&tmp);

    write_wal(
        &sessions_root,
        "01HZEXPORT002",
        &[dummy_receipt(), dummy_receipt()],
    );

    let exporter = Exporter::new(sessions_root, cs);
    let bytes = exporter.export_json("01HZEXPORT002").unwrap();

    assert!(
        serde_json::from_slice::<serde_json::Value>(&bytes).is_ok(),
        "export_json output must be valid JSON"
    );
}

// ---------------------------------------------------------------------------
// Tarball export
// ---------------------------------------------------------------------------

#[test]
fn store_02_1_tarball_contains_manifest_and_blobs() {
    let tmp = TempDir::new().unwrap();
    let sessions_root = tmp.path().join("sessions");
    fs::create_dir_all(&sessions_root).unwrap();
    let cs = content_store_fixture(&tmp);

    let r1 = cs.put(b"blob-content-alpha").unwrap();
    let r2 = cs.put(b"blob-content-beta").unwrap();

    // receipt_canonical_bytes = JSON containing the blob sha256 so the
    // exporter's collect_hex64_strings pass discovers them.
    let receipt1 = serde_json::to_vec(&serde_json::json!({"sha256": r1.sha256})).unwrap();
    let receipt2 = serde_json::to_vec(&serde_json::json!({"sha256": r2.sha256})).unwrap();

    write_wal(&sessions_root, "01HZEXPORT003", &[receipt1, receipt2]);

    // Write a manifest.json checkpoint (required by export_tarball).
    fs::write(
        sessions_root.join("01HZEXPORT003").join("manifest.json"),
        serde_json::to_vec(&serde_json::json!({"entries": []})).unwrap(),
    )
    .unwrap();

    let exporter = Exporter::new(sessions_root, cs);
    let tarball = exporter.export_tarball("01HZEXPORT003").unwrap();

    use flate2::read::GzDecoder;
    let cursor = std::io::Cursor::new(&tarball);
    let gz = GzDecoder::new(cursor);
    let mut archive = tar::Archive::new(gz);

    let entry_names: Vec<String> = archive
        .entries()
        .unwrap()
        .map(|e| e.unwrap().path().unwrap().to_str().unwrap().to_string())
        .collect();

    assert!(
        entry_names.contains(&"manifest.json".to_string()),
        "tarball must contain manifest.json; got: {entry_names:?}"
    );
    assert!(
        entry_names.contains(&format!("cas/{}", r1.sha256)),
        "tarball must contain cas blob 1"
    );
    assert!(
        entry_names.contains(&format!("cas/{}", r2.sha256)),
        "tarball must contain cas blob 2"
    );
}

#[test]
fn store_02_1_tarball_is_valid_gzip() {
    let tmp = TempDir::new().unwrap();
    let sessions_root = tmp.path().join("sessions");
    fs::create_dir_all(&sessions_root).unwrap();
    let cs = content_store_fixture(&tmp);

    write_wal(&sessions_root, "01HZEXPORT004", &[dummy_receipt()]);
    fs::write(
        sessions_root.join("01HZEXPORT004").join("manifest.json"),
        b"{}",
    )
    .unwrap();

    let exporter = Exporter::new(sessions_root, cs);
    let tarball = exporter.export_tarball("01HZEXPORT004").unwrap();

    use flate2::read::GzDecoder;
    let cursor = std::io::Cursor::new(&tarball);
    let gz = GzDecoder::new(cursor);
    let mut archive = tar::Archive::new(gz);

    assert!(
        archive.entries().is_ok(),
        "tarball must be a valid gzip-compressed tar archive"
    );
}

// ---------------------------------------------------------------------------
// HAR export
// ---------------------------------------------------------------------------

#[test]
fn cli_03_2_har_entries_length_matches_network_event_count_across_navigate_receipts() {
    // Two navigate receipts × 3 NetworkEvents each = 6 entries.
    // Note: this AC's text reads "network event count" verbatim (not
    // receipt count) — entries[] aggregates events across receipts.
    let tmp = TempDir::new().unwrap();
    let sessions_root = tmp.path().join("sessions");
    fs::create_dir_all(&sessions_root).unwrap();
    let cs = content_store_fixture(&tmp);

    let mk = |action: &str, base: &str| {
        navigate_receipt_bytes(
            action,
            base,
            200,
            vec![
                net_event("GET", &format!("{base}/a"), 200, 100, "text/html"),
                net_event("GET", &format!("{base}/b.css"), 200, 50, "text/css"),
                net_event(
                    "GET",
                    &format!("{base}/c.js"),
                    200,
                    75,
                    "application/javascript",
                ),
            ],
        )
    };
    let receipts = vec![mk("1", "https://a.example"), mk("2", "https://b.example")];
    write_wal(&sessions_root, "01HZEXPORT005", &receipts);

    let exporter = Exporter::new(sessions_root, cs);
    let bytes = exporter.export_har("01HZEXPORT005").unwrap();
    let har: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    let entries_len = har["log"]["entries"].as_array().unwrap().len();
    assert_eq!(
        entries_len, 6,
        "HAR entries count must equal sum of NetworkEvent counts"
    );
}

// ---------------------------------------------------------------------------
// HAR 1.2 schema validation (5 network events example)
// ---------------------------------------------------------------------------

#[test]
fn interop_02_1_har_schema_valid() {
    let tmp = TempDir::new().unwrap();
    let sessions_root = tmp.path().join("sessions");
    fs::create_dir_all(&sessions_root).unwrap();
    let cs = content_store_fixture(&tmp);

    let events = vec![
        net_event("GET", "https://example.com/", 200, 1024, "text/html"),
        net_event("GET", "https://example.com/style.css", 200, 256, "text/css"),
        net_event(
            "GET",
            "https://example.com/app.js",
            200,
            2048,
            "application/javascript",
        ),
        net_event(
            "GET",
            "https://example.com/logo.png",
            200,
            4096,
            "image/png",
        ),
        net_event(
            "GET",
            "https://example.com/api/data",
            200,
            512,
            "application/json",
        ),
    ];
    write_wal(
        &sessions_root,
        "01HZEXPORT006",
        &[navigate_receipt_bytes(
            "1",
            "https://example.com/",
            200,
            events,
        )],
    );

    let exporter = Exporter::new(sessions_root, cs);
    let bytes = exporter.export_har("01HZEXPORT006").unwrap();
    let har: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_har_12_valid(&har);
    assert_eq!(har["log"]["entries"].as_array().unwrap().len(), 5);
}

// ---------------------------------------------------------------------------
// Populated request/response fields per network event
// ---------------------------------------------------------------------------

#[test]
fn harexport_01_request_url_matches_navigated_url() {
    let tmp = TempDir::new().unwrap();
    let sessions_root = tmp.path().join("sessions");
    fs::create_dir_all(&sessions_root).unwrap();
    let cs = content_store_fixture(&tmp);

    let events = vec![net_event(
        "GET",
        "https://docs.example.com/guide",
        200,
        1234,
        "text/html",
    )];
    write_wal(
        &sessions_root,
        "01HZHAREXP01",
        &[navigate_receipt_bytes(
            "1",
            "https://docs.example.com/guide",
            200,
            events,
        )],
    );

    let exporter = Exporter::new(sessions_root, cs);
    let bytes = exporter.export_har("01HZHAREXP01").unwrap();
    let har: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    let entries = har["log"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "expected one entry per network event");
    assert_eq!(
        entries[0]["request"]["url"].as_str().unwrap(),
        "https://docs.example.com/guide",
        "request.url must reflect the navigated URL, not 'about:loom'"
    );
    assert_eq!(entries[0]["request"]["method"].as_str().unwrap(), "GET");
}

#[test]
fn harexport_02_response_status_matches_navigation() {
    let tmp = TempDir::new().unwrap();
    let sessions_root = tmp.path().join("sessions");
    fs::create_dir_all(&sessions_root).unwrap();
    let cs = content_store_fixture(&tmp);

    let events = vec![
        net_event("GET", "https://example.com/ok", 200, 10, "text/html"),
        net_event("GET", "https://example.com/missing", 404, 0, "text/html"),
        net_event("GET", "https://example.com/oops", 500, 0, "text/html"),
    ];
    write_wal(
        &sessions_root,
        "01HZHAREXP02",
        &[navigate_receipt_bytes(
            "1",
            "https://example.com/ok",
            200,
            events,
        )],
    );

    let exporter = Exporter::new(sessions_root, cs);
    let bytes = exporter.export_har("01HZHAREXP02").unwrap();
    let har: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    let entries = har["log"]["entries"].as_array().unwrap();
    assert_eq!(entries[0]["response"]["status"].as_u64().unwrap(), 200);
    assert_eq!(entries[1]["response"]["status"].as_u64().unwrap(), 404);
    assert_eq!(entries[2]["response"]["status"].as_u64().unwrap(), 500);
    assert_eq!(
        entries[1]["response"]["statusText"].as_str().unwrap(),
        "Not Found"
    );
}

#[test]
fn harexport_03_content_size_and_mimetype_populated() {
    let tmp = TempDir::new().unwrap();
    let sessions_root = tmp.path().join("sessions");
    fs::create_dir_all(&sessions_root).unwrap();
    let cs = content_store_fixture(&tmp);

    let events = vec![
        net_event("GET", "https://example.com/page", 200, 4096, "text/html"),
        net_event(
            "GET",
            "https://example.com/data.json",
            200,
            256,
            "application/json",
        ),
    ];
    write_wal(
        &sessions_root,
        "01HZHAREXP03",
        &[navigate_receipt_bytes(
            "1",
            "https://example.com/page",
            200,
            events,
        )],
    );

    let exporter = Exporter::new(sessions_root, cs);
    let bytes = exporter.export_har("01HZHAREXP03").unwrap();
    let har: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    let entries = har["log"]["entries"].as_array().unwrap();
    assert_eq!(
        entries[0]["response"]["content"]["size"].as_u64().unwrap(),
        4096
    );
    assert_eq!(
        entries[0]["response"]["content"]["mimeType"]
            .as_str()
            .unwrap(),
        "text/html"
    );
    assert_eq!(
        entries[1]["response"]["content"]["size"].as_u64().unwrap(),
        256
    );
    assert_eq!(
        entries[1]["response"]["content"]["mimeType"]
            .as_str()
            .unwrap(),
        "application/json"
    );
}

#[test]
fn harexport_04_har_still_validates_against_har_12_spec() {
    // Even after switching from placeholders to real per-event data, every
    // HAR 1.2 required field must remain present.
    let tmp = TempDir::new().unwrap();
    let sessions_root = tmp.path().join("sessions");
    fs::create_dir_all(&sessions_root).unwrap();
    let cs = content_store_fixture(&tmp);

    let events = vec![
        net_event("GET", "https://example.com/", 200, 1, "text/html"),
        net_event(
            "POST",
            "https://example.com/api",
            201,
            2,
            "application/json",
        ),
    ];
    write_wal(
        &sessions_root,
        "01HZHAREXP04",
        &[navigate_receipt_bytes(
            "1",
            "https://example.com/",
            200,
            events,
        )],
    );

    let exporter = Exporter::new(sessions_root, cs);
    let bytes = exporter.export_har("01HZHAREXP04").unwrap();
    let har: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_har_12_valid(&har);
}

#[test]
fn harexport_05_entries_length_matches_network_event_count_not_receipt_count() {
    // 1 navigate receipt with 4 NetworkEvents + 1 navigate receipt with 0
    // events + 1 click (no network) → 4 entries (per network-event count,
    // NOT 3 receipts).
    let tmp = TempDir::new().unwrap();
    let sessions_root = tmp.path().join("sessions");
    fs::create_dir_all(&sessions_root).unwrap();
    let cs = content_store_fixture(&tmp);

    let nav4 = navigate_receipt_bytes(
        "1",
        "https://a.example",
        200,
        vec![
            net_event("GET", "https://a.example/1", 200, 1, "text/html"),
            net_event("GET", "https://a.example/2", 200, 2, "text/css"),
            net_event(
                "GET",
                "https://a.example/3",
                200,
                3,
                "application/javascript",
            ),
            net_event("GET", "https://a.example/4", 200, 4, "image/png"),
        ],
    );
    let nav0 = navigate_receipt_bytes("2", "https://b.example", 200, Vec::new());
    write_wal(
        &sessions_root,
        "01HZHAREXP05",
        &[nav4, nav0, dummy_receipt()],
    );

    let exporter = Exporter::new(sessions_root, cs);
    let bytes = exporter.export_har("01HZHAREXP05").unwrap();
    let har: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(
        har["log"]["entries"].as_array().unwrap().len(),
        4,
        "entries[] length must equal total NetworkEvent count, not receipt count"
    );
}

// ---------------------------------------------------------------------------
// HAR 1.2 schema checker (manual field validation per spec)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Validate against the vendored HAR 1.2 JSON Schema
// (true Draft-04 schema validation via the `jsonschema` crate, in addition
// to the manual `assert_har_12_valid` checker above). The vendored fixture
// at tests/fixtures/har-1.2.schema.json carries provenance metadata in its
// top-level `$comment` field.
// ---------------------------------------------------------------------------

fn load_har_schema() -> serde_json::Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("har-1.2.schema.json");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

#[test]
fn harexport_04_validates_against_har12_schema() {
    let tmp = TempDir::new().unwrap();
    let sessions_root = tmp.path().join("sessions");
    fs::create_dir_all(&sessions_root).unwrap();
    let cs = content_store_fixture(&tmp);

    // Two events to exercise both 2xx and the explicit content/timings fields.
    let events = vec![
        net_event("GET", "https://example.com/", 200, 1024, "text/html"),
        net_event("GET", "https://example.com/style.css", 200, 256, "text/css"),
    ];
    write_wal(
        &sessions_root,
        "01HZHAREXP04S",
        &[navigate_receipt_bytes(
            "1",
            "https://example.com/",
            200,
            events,
        )],
    );

    let exporter = Exporter::new(sessions_root, cs);
    let bytes = exporter.export_har("01HZHAREXP04S").unwrap();
    let har: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    let schema = load_har_schema();
    let validator = jsonschema::validator_for(&schema)
        .expect("vendored HAR 1.2 schema must compile as a JSON Schema");

    let errors: Vec<String> = validator
        .iter_errors(&har)
        .map(|e| format!("{e}"))
        .collect();
    assert!(
        errors.is_empty(),
        "HAR export must validate against vendored HAR 1.2 schema; \
         got {} error(s):\n{}",
        errors.len(),
        errors.join("\n")
    );
}

#[test]
fn harexport_fixture_is_valid_json_schema() {
    // Defends against `jsonschema` crate upgrades silently breaking the
    // vendored schema fixture. If a future bump (0.46 → 0.50, …) refuses
    // the Draft-04 form, we want this test to fail loud rather than the
    // schema-validation test silently accepting any HAR shape.
    let schema = load_har_schema();
    jsonschema::validator_for(&schema).expect(
        "meta-schema check: the vendored HAR 1.2 schema must compile \
         as a valid JSON Schema (Draft-04). If this fails after a `jsonschema` crate \
         upgrade, update the fixture to the form the new crate expects rather than \
         relaxing this test.",
    );
}

fn assert_har_12_valid(har: &serde_json::Value) {
    let log = har["log"].as_object().expect("HAR must have 'log' object");
    assert_eq!(
        log["version"].as_str().unwrap(),
        "1.2",
        "HAR version must be '1.2'"
    );

    let creator = log["creator"]
        .as_object()
        .expect("HAR must have 'creator' object");
    assert!(creator.contains_key("name"), "creator must have 'name'");
    assert!(
        creator.contains_key("version"),
        "creator must have 'version'"
    );

    let entries = log["entries"]
        .as_array()
        .expect("HAR must have 'entries' array");
    for (i, entry) in entries.iter().enumerate() {
        let e = entry.as_object().expect("HAR entry must be an object");

        assert!(
            e["startedDateTime"].is_string(),
            "entry[{i}] must have startedDateTime"
        );
        assert!(e["time"].is_number(), "entry[{i}] must have time");

        let req = e["request"]
            .as_object()
            .expect("entry[{i}] must have request");
        assert!(req["method"].is_string(), "request.method");
        assert!(req["url"].is_string(), "request.url");
        assert!(req["httpVersion"].is_string(), "request.httpVersion");
        assert!(req["cookies"].is_array(), "request.cookies");
        assert!(req["headers"].is_array(), "request.headers");
        assert!(req["queryString"].is_array(), "request.queryString");
        assert!(req["headersSize"].is_number(), "request.headersSize");
        assert!(req["bodySize"].is_number(), "request.bodySize");

        let resp = e["response"]
            .as_object()
            .expect("entry[{i}] must have response");
        assert!(resp["status"].is_number(), "response.status");
        assert!(resp["statusText"].is_string(), "response.statusText");
        assert!(resp["httpVersion"].is_string(), "response.httpVersion");
        assert!(resp["cookies"].is_array(), "response.cookies");
        assert!(resp["headers"].is_array(), "response.headers");
        let content = resp["content"]
            .as_object()
            .expect("response must have content");
        assert!(content["size"].is_number(), "content.size");
        assert!(content["mimeType"].is_string(), "content.mimeType");
        assert!(resp["redirectURL"].is_string(), "response.redirectURL");
        assert!(resp["headersSize"].is_number(), "response.headersSize");
        assert!(resp["bodySize"].is_number(), "response.bodySize");

        assert!(e.contains_key("cache"), "entry[{i}] must have cache");

        let timings = e["timings"]
            .as_object()
            .expect("entry[{i}] must have timings");
        assert!(timings["send"].is_number(), "timings.send");
        assert!(timings["wait"].is_number(), "timings.wait");
        assert!(timings["receive"].is_number(), "timings.receive");
    }
}
