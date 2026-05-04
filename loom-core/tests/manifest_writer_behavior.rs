// Behavior tests for LocalManifestWriter.
//
// AC coverage:
//   AC-CORE-03.1 manifest.json public checkpoint: test_manifest_json_checkpoint_has_entries_array
//   AC-CORE-06.1 hash chain (IC-CORE-04): test_manifest_hash_chain_validates_intact,
//                                         test_manifest_hash_chain_detects_tampering
//   AC-CORE-06.2 structured error context: test_validate_corrupt_error_has_structured_context
//   AC-NFR-REL-01.1 atomic write:          test_manifest_atomic_write_no_tmp_file
//   BC HARD #3 JCS compliance:             test_manifest_jcs_not_serde_json

use loom_core::error::LoomErrorCode;
use loom_core::manifest_writer::{
    AuditKind, LocalManifestWriter, ManifestEntry, ManifestWriter, SessionId,
};
use loom_core::observability::Observability;
use std::path::PathBuf;

fn make_mw(tmp: &str) -> LocalManifestWriter {
    let obs = Observability::new(PathBuf::from(format!("{tmp}/loom.log")), false);
    LocalManifestWriter::new(PathBuf::from(format!("{tmp}/sessions")), obs)
}

// === IC-CORE-04: hash chain integrity ===

#[test]
fn test_manifest_hash_chain_validates_intact() {
    let tmp = "/tmp/loom-test-mw-validate";
    let mw = make_mw(tmp);
    let sid = SessionId("01HZAAAAAAAAAAAAAAAAAAAAAA".into());
    let handle = mw.open_manifest(sid.clone(), None).unwrap();
    let _ = handle;
    mw.append(
        sid.clone(),
        ManifestEntry::ActionReceipt {
            action_id: 1,
            emitted_at_ms: 1_714_000_000_000,
            receipt_canonical_bytes: b"action1".to_vec(),
            prev_hash: "0".repeat(64),
        },
    )
    .unwrap();
    mw.append(
        sid.clone(),
        ManifestEntry::ActionReceipt {
            action_id: 2,
            emitted_at_ms: 1_714_000_001_000,
            receipt_canonical_bytes: b"action2".to_vec(),
            prev_hash: "0".repeat(64),
        },
    )
    .unwrap();
    // Intact chain must validate without error.
    mw.validate(sid)
        .expect("intact hash chain must validate as Ok");
}

#[test]
fn test_manifest_hash_chain_detects_tampering() {
    let tmp = "/tmp/loom-test-mw-tamper";
    let mw = make_mw(tmp);
    let sid = SessionId("01HZBBBBBBBBBBBBBBBBBBBBBB".into());
    mw.open_manifest(sid.clone(), None).unwrap();
    mw.append(
        sid.clone(),
        ManifestEntry::ActionReceipt {
            action_id: 1,
            emitted_at_ms: 1_714_000_000_000,
            receipt_canonical_bytes: b"action1".to_vec(),
            prev_hash: "0".repeat(64),
        },
    )
    .unwrap();
    // Tamper: overwrite the WAL file with a corrupted second line.
    let wal_path = PathBuf::from(format!("{tmp}/sessions/{}/manifest.wal", sid.0));
    let mut contents = std::fs::read_to_string(&wal_path).unwrap();
    contents.push_str("{\"kind\":\"action_receipt\",\"action_id\":99,\"emitted_at_ms\":0,\"receipt_canonical_bytes\":[],\"prev_hash\":\"deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef\"}\n");
    std::fs::write(&wal_path, contents).unwrap();
    // Tampered chain must return ManifestCorrupt.
    let err = mw.validate(sid).unwrap_err();
    assert_eq!(err.code, LoomErrorCode::ManifestCorrupt);
}

// === AC-CORE-03.1: manifest.json public checkpoint ===

#[test]
fn test_manifest_json_checkpoint_has_entries_array() {
    let tmp = "/tmp/loom-test-mw-json-ckpt";
    let sid = SessionId("01HZDDDDDDDDDDDDDDDDDDDDDD".into());
    // Clean up previous run so open_manifest creates a fresh WAL.
    std::fs::remove_dir_all(format!("{tmp}/sessions/{}", sid.0)).ok();

    let mw = make_mw(tmp);
    mw.open_manifest(sid.clone(), None).unwrap();
    mw.append(
        sid.clone(),
        ManifestEntry::ActionReceipt {
            action_id: 1,
            emitted_at_ms: 1_714_000_000_000,
            receipt_canonical_bytes: b"receipt-bytes-1".to_vec(),
            prev_hash: "0".repeat(64),
        },
    )
    .unwrap();
    mw.append(
        sid.clone(),
        ManifestEntry::ActionReceipt {
            action_id: 2,
            emitted_at_ms: 1_714_000_001_000,
            receipt_canonical_bytes: b"receipt-bytes-2".to_vec(),
            prev_hash: "0".repeat(64),
        },
    )
    .unwrap();
    // SessionTerminal append must auto-trigger export of manifest.json.
    mw.append(
        sid.clone(),
        ManifestEntry::SessionTerminal {
            action_id: 3,
            emitted_at_ms: 1_714_000_002_000,
            reason: "session_closed".into(),
            prev_hash: "0".repeat(64),
        },
    )
    .unwrap();

    let json_path = PathBuf::from(format!("{tmp}/sessions/{}/manifest.json", sid.0));
    assert!(
        json_path.exists(),
        "manifest.json must exist after SessionTerminal append"
    );

    let contents = std::fs::read_to_string(&json_path).unwrap();
    let doc: serde_json::Value =
        serde_json::from_str(&contents).expect("manifest.json must be valid JSON");

    let entries = doc["entries"]
        .as_array()
        .expect("manifest.json must have top-level 'entries' array");
    assert_eq!(
        entries.len(),
        2,
        "entries[] must contain 2 ActionReceipt entries (Terminal excluded)"
    );

    for entry in entries {
        assert!(
            entry.get("action_id").is_some(),
            "entry must have 'action_id'"
        );
        assert!(
            entry.get("action").is_some(),
            "entry must have 'action' field"
        );
        assert!(
            entry.get("receipt").is_some(),
            "entry must have 'receipt' field"
        );
        let content_refs = entry["content_refs"]
            .as_array()
            .expect("'content_refs' must be a JSON array");
        assert!(
            content_refs.is_empty(),
            "content_refs must be [] for this feature"
        );
    }
}

// === AC-CORE-06.2: structured error context on hash chain break ===

#[test]
fn test_validate_corrupt_error_has_structured_context() {
    let tmp = "/tmp/loom-test-mw-ctx";
    let sid = SessionId("01HZEEEEEEEEEEEEEEEEEEEEEE".into());
    std::fs::remove_dir_all(format!("{tmp}/sessions/{}", sid.0)).ok();

    let mw = make_mw(tmp);
    mw.open_manifest(sid.clone(), None).unwrap();
    mw.append(
        sid.clone(),
        ManifestEntry::ActionReceipt {
            action_id: 1,
            emitted_at_ms: 1_714_000_000_000,
            receipt_canonical_bytes: b"action1".to_vec(),
            prev_hash: "0".repeat(64),
        },
    )
    .unwrap();
    // Append a line with a deliberately wrong prev_hash.
    let wal_path = PathBuf::from(format!("{tmp}/sessions/{}/manifest.wal", sid.0));
    let mut contents = std::fs::read_to_string(&wal_path).unwrap();
    contents.push_str("{\"action_id\":99,\"emitted_at_ms\":0,\"kind\":\"action_receipt\",\"prev_hash\":\"deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef\",\"receipt_canonical_bytes\":[]}\n");
    std::fs::write(&wal_path, contents).unwrap();

    let err = mw.validate(sid).unwrap_err();
    assert_eq!(err.code, LoomErrorCode::ManifestCorrupt);

    // AC-CORE-06.2: context must carry failed_at_index, expected_hash, observed_hash.
    let ctx = err
        .context
        .expect("ManifestCorrupt must include structured context");
    assert!(
        ctx.get("failed_at_index").is_some(),
        "context must have 'failed_at_index'"
    );
    let expected = ctx["expected_hash"]
        .as_str()
        .expect("context must have 'expected_hash' string");
    let observed = ctx["observed_hash"]
        .as_str()
        .expect("context must have 'observed_hash' string");
    assert_ne!(
        expected, observed,
        "expected_hash must differ from observed_hash on tamper"
    );
}

// === AC-NFR-REL-01.1: manifest.json atomic write ===

#[test]
fn test_manifest_atomic_write_no_tmp_file() {
    let tmp = "/tmp/loom-test-mw-atomic";
    let sid = SessionId("01HZFFFFFFFFFFFFFFFFFFFFFF".into());
    std::fs::remove_dir_all(format!("{tmp}/sessions/{}", sid.0)).ok();

    let mw = make_mw(tmp);
    mw.open_manifest(sid.clone(), None).unwrap();
    mw.append(
        sid.clone(),
        ManifestEntry::ActionReceipt {
            action_id: 1,
            emitted_at_ms: 1_714_000_000_000,
            receipt_canonical_bytes: b"some-receipt".to_vec(),
            prev_hash: "0".repeat(64),
        },
    )
    .unwrap();
    // Trigger manifest.json export directly (also tested via SessionTerminal above).
    mw.export_manifest_json(sid.clone()).unwrap();

    let json_path = PathBuf::from(format!("{tmp}/sessions/{}/manifest.json", sid.0));
    let tmp_path = PathBuf::from(format!("{tmp}/sessions/{}/manifest.json.tmp", sid.0));

    // manifest.json must exist and be valid JSON.
    assert!(json_path.exists(), "manifest.json must exist after export");
    let contents = std::fs::read_to_string(&json_path).unwrap();
    serde_json::from_str::<serde_json::Value>(&contents)
        .expect("manifest.json must be valid JSON (complete write)");

    // manifest.json.tmp must NOT exist — rename must have completed.
    assert!(
        !tmp_path.exists(),
        "manifest.json.tmp must not exist after successful export (atomic rename completed)"
    );
}

// === BC HARD #3: JCS serialization (not serde_json::to_string) ===

#[test]
fn test_manifest_jcs_not_serde_json() {
    // Verify that the WAL entry is serialized with sorted JSON keys (JCS).
    // serde_json::to_string does NOT guarantee key ordering.
    // serde_jcs::to_string sorts keys alphabetically — that's the contract.
    let tmp = "/tmp/loom-test-mw-jcs";
    let mw = make_mw(tmp);
    let sid = SessionId("01HZCCCCCCCCCCCCCCCCCCCCCC".into());
    mw.open_manifest(sid.clone(), None).unwrap();
    mw.append(
        sid.clone(),
        ManifestEntry::AuditEntry {
            action_id_ref: Some(1),
            emitted_at_ms: 1_714_000_000_000,
            audit_kind: AuditKind::GrantIssued,
            canonical_bytes: vec![],
            prev_hash: "0".repeat(64),
        },
    )
    .unwrap();
    let wal_path = PathBuf::from(format!("{tmp}/sessions/{}/manifest.wal", sid.0));
    let contents = std::fs::read_to_string(&wal_path).unwrap();
    // The second line (index 1) is the AuditEntry we just appended.
    let entry_line = contents.lines().nth(1).expect("second WAL line must exist");
    // In JCS the keys are sorted — "action_id_ref" sorts before "audit_kind"
    // which sorts before "canonical_bytes", etc. Verify "action_id_ref" appears
    // before "audit_kind" in the raw JSON string.
    let pos_action = entry_line.find("action_id_ref").unwrap();
    let pos_audit = entry_line.find("audit_kind").unwrap();
    assert!(
        pos_action < pos_audit,
        "JCS requires keys sorted: 'action_id_ref' must precede 'audit_kind' in WAL output"
    );
}
