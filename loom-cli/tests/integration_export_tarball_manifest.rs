//! `loom session export --format tarball` output-contract golden test
//! (CLI-level).
//!
//! Regression: the tarball's `manifest.json` was literally `{}` (the
//! exporter read a session-dir checkpoint that only exists after a clean
//! close), so a tarball alone could not identify what session/actions it
//! contained. The contract is: the tarball's `manifest.json` carries the
//! same self-describing document as `--format json`
//! (`{"manifest":{"session_id":…,"started_at_ms":…,"actions":[…]},
//! "content_blob_index":[…]}`) — i.e.
//! `tar xOf s.tar manifest.json | jq .manifest.session_id` returns the id.
//!
//! The session is fabricated directly on disk under the daemon's
//! `<data_root>/sessions/<sid>/manifest.wal` (same fixture approach as
//! loom-core's `session_export_behavior.rs` — export is a pure WAL + CAS
//! read, so no browser is needed).

mod common;

use common::daemon_test_harness::DaemonTestHarness;
use loom_core::manifest_writer::ManifestEntry;
use std::io::Read as _;

/// 26-char lowercase-alnum session id (the daemon's path-traversal gate
/// rejects anything else before the exporter runs).
const SID: &str = "01hzexporttarball000000000";

#[test]
fn tarball_manifest_json_matches_json_export_and_names_the_session() {
    let h0 = DaemonTestHarness::new();
    let data_root = h0.home().join("loom-data");
    let sessions_root = data_root.join("sessions");
    std::fs::create_dir_all(&sessions_root).unwrap();
    let mut h = h0
        .env("LOOM_DATA_ROOT", &data_root)
        .env("LOOM_AUTH_DIR", data_root.join("auth"))
        .env("LOOM_REAPER_SWEEP_SECS", "300");
    h.start();

    // Fixture written AFTER start so the daemon's startup crash-recovery
    // sweep can't checkpoint/quarantine it first: Header + one ActionReceipt
    // (no content refs → no CAS blobs required).
    let entries = [
        ManifestEntry::Header {
            session_id: SID.to_string(),
            started_at_ms: 1234,
            prev_hash: None,
            budgets: None,
            capture_policy: None,
            seed: None,
            determinism_enabled: None,
        },
        ManifestEntry::ActionReceipt {
            action_id: 1,
            emitted_at_ms: 2000,
            receipt_canonical_bytes: serde_json::to_vec(
                &serde_json::json!({"op": "click", "target": "#btn"}),
            )
            .unwrap(),
            prev_hash: "0".repeat(64),
        },
    ];
    let dir = sessions_root.join(SID);
    std::fs::create_dir_all(&dir).unwrap();
    let lines: Vec<String> = entries
        .iter()
        .map(|e| serde_json::to_string(e).unwrap())
        .collect();
    std::fs::write(dir.join("manifest.wal"), lines.join("\n")).unwrap();

    // Export both formats through the real CLI → daemon → exporter path.
    let tar_path = h.home().join("s.tar");
    let out = h
        .loom_command()
        .args(["session", "export", SID, "--format", "tarball", "--output"])
        .arg(&tar_path)
        .output()
        .expect("run loom session export --format tarball");
    assert!(
        out.status.success(),
        "tarball export must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let json_path = h.home().join("s.json");
    let out = h
        .loom_command()
        .args(["session", "export", SID, "--format", "json", "--output"])
        .arg(&json_path)
        .output()
        .expect("run loom session export --format json");
    assert!(
        out.status.success(),
        "json export must exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json_export = std::fs::read(&json_path).unwrap();

    // Unpack manifest.json from the (gzip-compressed) tarball.
    let tarball = std::fs::read(&tar_path).unwrap();
    let gz = flate2::read::GzDecoder::new(std::io::Cursor::new(&tarball));
    let mut archive = tar::Archive::new(gz);
    let mut manifest_bytes = Vec::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        if entry.path().unwrap().to_str() == Some("manifest.json") {
            entry.read_to_end(&mut manifest_bytes).unwrap();
        }
    }
    assert!(
        !manifest_bytes.is_empty(),
        "tarball must contain a manifest.json entry"
    );

    // The archive's manifest is the SAME document as the json export…
    assert_eq!(
        manifest_bytes,
        json_export,
        "tarball manifest.json must match the --format json export; got: {:?}",
        String::from_utf8_lossy(&manifest_bytes)
    );

    // …and is self-describing: `jq .manifest.session_id` style access works.
    let doc: serde_json::Value =
        serde_json::from_slice(&manifest_bytes).expect("tarball manifest.json must parse as JSON");
    assert_eq!(doc["manifest"]["session_id"], serde_json::json!(SID));
    assert_eq!(doc["manifest"]["started_at_ms"], serde_json::json!(1234));
    let actions = doc["manifest"]["actions"]
        .as_array()
        .expect("manifest.actions must be an array");
    assert_eq!(actions.len(), 1, "got: {doc}");
    assert_eq!(actions[0]["action_id"], serde_json::json!(1));
}
