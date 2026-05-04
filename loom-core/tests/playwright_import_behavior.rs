//! AC-driven integration tests for the `playwright-import` feature.
//!
//! AC-INTEROP-01.1 — Playwright trace import produces a Loom session
//!   Given a Playwright trace `trace.zip` with K events,
//!   When `PlaywrightImporter::import(trace_bytes)` is called,
//!   Then a new session is created with `actions[]` length = K;
//!   each action's `source = "playwright_import"`; `session.replayable = false`.

use loom_core::importers::PlaywrightImporter;
use loom_core::manifest_writer::ManifestEntry;
use std::io::Write as _;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

/// Build an in-memory Playwright trace.zip with `events` lines in `trace.trace`.
fn make_trace_zip(events: &[&str]) -> Vec<u8> {
    use std::io::Cursor;
    use zip::write::{FileOptions, ZipWriter};

    let buf = Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(buf);
    let opts: FileOptions<()> = FileOptions::default();
    zip.start_file("trace.trace", opts).unwrap();
    for event in events {
        zip.write_all(event.as_bytes()).unwrap();
        zip.write_all(b"\n").unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn sample_events(k: usize) -> Vec<String> {
    (0..k)
        .map(|i| format!(r#"{{"type":"action","callId":"pw:api:{}","startTime":{},"endTime":{}}}"#, i, i * 10, i * 10 + 5))
        .collect()
}

fn read_wal_entries(sessions_root: &std::path::Path, session_id: &str) -> Vec<ManifestEntry> {
    let wal_path = sessions_root.join(session_id).join("manifest.wal");
    let content = std::fs::read_to_string(&wal_path).expect("manifest.wal must exist");
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("each WAL line must be valid ManifestEntry JSON"))
        .collect()
}

fn action_receipts(entries: &[ManifestEntry]) -> Vec<&ManifestEntry> {
    entries
        .iter()
        .filter(|e| matches!(e, ManifestEntry::ActionReceipt { .. }))
        .collect()
}

// ---------------------------------------------------------------------------
// AC-INTEROP-01.1 — actions[] length = K
// ---------------------------------------------------------------------------

#[test]
fn ac_interop_01_1_action_count_equals_k_events() {
    let tmp = TempDir::new().unwrap();
    let sessions_root = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_root).unwrap();

    let k = 7;
    let events: Vec<String> = sample_events(k);
    let event_strs: Vec<&str> = events.iter().map(String::as_str).collect();
    let trace_zip = make_trace_zip(&event_strs);

    let importer = PlaywrightImporter::new(sessions_root.clone());
    let result = importer.import(&trace_zip).expect("import must succeed");

    assert_eq!(result.action_count, k as u64, "action_count must equal K");

    let entries = read_wal_entries(&sessions_root, &result.session_id);
    let receipts = action_receipts(&entries);
    assert_eq!(
        receipts.len(),
        k,
        "WAL must have exactly K ActionReceipt entries; got {}",
        receipts.len()
    );

    // Session dir must exist on disk.
    assert!(
        sessions_root.join(&result.session_id).exists(),
        "session directory must be created on disk"
    );
}

// ---------------------------------------------------------------------------
// AC-INTEROP-01.1 — each action.source = "playwright_import"
// ---------------------------------------------------------------------------

#[test]
fn ac_interop_01_1_each_action_source_is_playwright_import() {
    let tmp = TempDir::new().unwrap();
    let sessions_root = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_root).unwrap();

    let events: Vec<String> = sample_events(3);
    let event_strs: Vec<&str> = events.iter().map(String::as_str).collect();
    let trace_zip = make_trace_zip(&event_strs);

    let importer = PlaywrightImporter::new(sessions_root.clone());
    let result = importer.import(&trace_zip).unwrap();

    let entries = read_wal_entries(&sessions_root, &result.session_id);
    for entry in &entries {
        if let ManifestEntry::ActionReceipt { receipt_canonical_bytes, .. } = entry {
            let val: serde_json::Value =
                serde_json::from_slice(receipt_canonical_bytes).expect("receipt bytes must be valid JSON");
            assert_eq!(
                val["source"].as_str(),
                Some("playwright_import"),
                "each action receipt must have source = 'playwright_import'; got: {val}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// AC-INTEROP-01.1 — session.replayable = false
// ---------------------------------------------------------------------------

#[test]
fn ac_interop_01_1_session_replayable_false() {
    let tmp = TempDir::new().unwrap();
    let sessions_root = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_root).unwrap();

    let events: Vec<String> = sample_events(2);
    let event_strs: Vec<&str> = events.iter().map(String::as_str).collect();
    let trace_zip = make_trace_zip(&event_strs);

    let importer = PlaywrightImporter::new(sessions_root.clone());
    let result = importer.import(&trace_zip).unwrap();

    let meta_path = sessions_root.join(&result.session_id).join("session_meta.json");
    assert!(meta_path.exists(), "session_meta.json must be written");

    let meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&meta_path).unwrap())
            .expect("session_meta.json must be valid JSON");

    assert_eq!(
        meta["replayable"].as_bool(),
        Some(false),
        "session_meta.json must have replayable: false; got: {meta}"
    );
    assert_eq!(meta["source"].as_str(), Some("playwright_import"));
    assert_eq!(meta["action_count"].as_u64(), Some(2));
}

// ---------------------------------------------------------------------------
// AC-INTEROP-01.1 — edge case: K=0 events
// ---------------------------------------------------------------------------

#[test]
fn ac_interop_01_1_empty_trace_creates_zero_action_session() {
    let tmp = TempDir::new().unwrap();
    let sessions_root = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_root).unwrap();

    // trace.trace file exists but has no non-empty lines.
    let trace_zip = make_trace_zip(&[]);

    let importer = PlaywrightImporter::new(sessions_root.clone());
    let result = importer.import(&trace_zip).unwrap();

    assert_eq!(result.action_count, 0);

    let entries = read_wal_entries(&sessions_root, &result.session_id);
    let receipts = action_receipts(&entries);
    assert_eq!(receipts.len(), 0, "K=0 trace must produce 0 ActionReceipt entries");

    // WAL must still have a Header and a SessionTerminal.
    assert!(
        entries.iter().any(|e| matches!(e, ManifestEntry::Header { .. })),
        "WAL must have Header even for K=0"
    );
    assert!(
        entries.iter().any(|e| matches!(e, ManifestEntry::SessionTerminal { .. })),
        "WAL must have SessionTerminal even for K=0"
    );
}

// ---------------------------------------------------------------------------
// AC-INTEROP-01.1 — invalid zip bytes → Err
// ---------------------------------------------------------------------------

#[test]
fn ac_interop_01_1_invalid_zip_returns_error() {
    let tmp = TempDir::new().unwrap();
    let sessions_root = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_root).unwrap();

    let not_a_zip = b"this is definitely not a zip file";

    let importer = PlaywrightImporter::new(sessions_root.clone());
    let result = importer.import(not_a_zip);

    assert!(result.is_err(), "invalid zip must return Err");
}

// ---------------------------------------------------------------------------
// AC-INTEROP-01.1 — zip missing trace.trace → Err
// ---------------------------------------------------------------------------

#[test]
fn ac_interop_01_1_missing_trace_file_in_zip() {
    use std::io::Cursor;
    use zip::write::{FileOptions, ZipWriter};

    let tmp = TempDir::new().unwrap();
    let sessions_root = tmp.path().join("sessions");
    std::fs::create_dir_all(&sessions_root).unwrap();

    // Zip with a different file name, not trace.trace.
    let buf = Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(buf);
    let opts: FileOptions<()> = FileOptions::default();
    zip.start_file("network.trace", opts).unwrap();
    zip.write_all(b"{}").unwrap();
    let trace_zip = zip.finish().unwrap().into_inner();

    let importer = PlaywrightImporter::new(sessions_root.clone());
    let result = importer.import(&trace_zip);

    assert!(result.is_err(), "zip without trace.trace must return Err");
}
