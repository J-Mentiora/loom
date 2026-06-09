// Behavior tests for LocalManifestWriter.
//
// Coverage:
//   - manifest.json public checkpoint: test_manifest_json_checkpoint_has_entries_array
//   - hash chain: test_manifest_hash_chain_validates_intact,
//                 test_manifest_hash_chain_detects_tampering
//   - structured error context: test_validate_corrupt_error_has_structured_context
//   - atomic write: test_manifest_atomic_write_no_tmp_file
//   - JCS compliance (HARD #3): test_manifest_jcs_not_serde_json
//
// Each test gets its own `tempfile::TempDir` (auto-removed on drop) so a stale WAL
// can never leak across runs — bind the returned dir (`let (mw, _tmp) = make_mw();`)
// so it outlives the test body.

use loom_core::error::LoomErrorCode;
use loom_core::manifest_writer::{
    AuditKind, LocalManifestWriter, ManifestEntry, ManifestWriter, SessionId,
};
use loom_core::observability::Observability;
use tempfile::TempDir;

fn make_mw() -> (LocalManifestWriter, TempDir) {
    let tmp = TempDir::new().unwrap();
    let obs = Observability::new(tmp.path().join("loom.log"), false);
    let mw = LocalManifestWriter::new(tmp.path().join("sessions"), obs);
    (mw, tmp)
}

// === hash chain integrity ===

#[test]
fn test_manifest_hash_chain_validates_intact() {
    let (mw, _tmp) = make_mw();
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
    let (mw, tmp) = make_mw();
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
    let wal_path = tmp
        .path()
        .join("sessions")
        .join(&sid.0)
        .join("manifest.wal");
    let mut contents = std::fs::read_to_string(&wal_path).unwrap();
    contents.push_str("{\"kind\":\"action_receipt\",\"action_id\":99,\"emitted_at_ms\":0,\"receipt_canonical_bytes\":[],\"prev_hash\":\"deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef\"}\n");
    std::fs::write(&wal_path, contents).unwrap();
    // Tampered chain must return ManifestCorrupt.
    let err = mw.validate(sid).unwrap_err();
    assert_eq!(err.code, LoomErrorCode::ManifestCorrupt);
}

// === manifest.json public checkpoint ===

#[test]
fn test_manifest_json_checkpoint_has_entries_array() {
    let (mw, tmp) = make_mw();
    let sid = SessionId("01HZDDDDDDDDDDDDDDDDDDDDDD".into());
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

    let json_path = tmp
        .path()
        .join("sessions")
        .join(&sid.0)
        .join("manifest.json");
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

// === structured error context on hash chain break ===

#[test]
fn test_validate_corrupt_error_has_structured_context() {
    let (mw, tmp) = make_mw();
    let sid = SessionId("01HZEEEEEEEEEEEEEEEEEEEEEE".into());
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
    let wal_path = tmp
        .path()
        .join("sessions")
        .join(&sid.0)
        .join("manifest.wal");
    let mut contents = std::fs::read_to_string(&wal_path).unwrap();
    contents.push_str("{\"action_id\":99,\"emitted_at_ms\":0,\"kind\":\"action_receipt\",\"prev_hash\":\"deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef\",\"receipt_canonical_bytes\":[]}\n");
    std::fs::write(&wal_path, contents).unwrap();

    let err = mw.validate(sid).unwrap_err();
    assert_eq!(err.code, LoomErrorCode::ManifestCorrupt);

    // Context must carry failed_at_index, expected_hash, observed_hash.
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

// === manifest.json atomic write ===

#[test]
fn test_manifest_atomic_write_no_tmp_file() {
    let (mw, tmp) = make_mw();
    let sid = SessionId("01HZFFFFFFFFFFFFFFFFFFFFFF".into());
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

    let json_path = tmp
        .path()
        .join("sessions")
        .join(&sid.0)
        .join("manifest.json");
    let tmp_path = tmp
        .path()
        .join("sessions")
        .join(&sid.0)
        .join("manifest.json.tmp");

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
    let (mw, tmp) = make_mw();
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
    let wal_path = tmp
        .path()
        .join("sessions")
        .join(&sid.0)
        .join("manifest.wal");
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

// === incremental last-line cache (perf fix: O(n^2) full-WAL re-read) ===

// Build a session with three ActionReceipts. Returns the prev_hash of the
// LAST appended entry (entry 3), read back from the WAL. If `inject_bogus` is
// set, a bogus raw line is written into the WAL file OUT OF BAND between the
// 2nd and 3rd appends — simulating arbitrary unrelated file content that a
// full-file re-read would (incorrectly) chain off of.
fn build_three_and_read_last_prev_hash(tmp: &str, sid: SessionId, inject_bogus: bool) -> String {
    // Hermetic: drop any stale WAL from a previous run (the existing tests in
    // this file share /tmp; clean our session dir so open_manifest creates fresh).
    std::fs::remove_dir_all(format!("{tmp}/sessions/{}", sid.0)).ok();
    let mw = make_mw(tmp);
    mw.open_manifest(sid.clone(), None).unwrap();
    let receipt = |id: u64| ManifestEntry::ActionReceipt {
        action_id: id,
        emitted_at_ms: 1_714_000_000_000 + id,
        receipt_canonical_bytes: format!("action{id}").into_bytes(),
        prev_hash: "0".repeat(64), // overwritten by append()
    };
    mw.append(sid.clone(), receipt(1)).unwrap();
    mw.append(sid.clone(), receipt(2)).unwrap();

    if inject_bogus {
        // Append a bogus line directly to the file, bypassing append(). A
        // full-file re-read would see THIS as the last line; the per-session
        // cache must ignore it and chain entry 3 off entry 2.
        let wal_path = PathBuf::from(format!("{tmp}/sessions/{}/manifest.wal", sid.0));
        let mut contents = std::fs::read_to_string(&wal_path).unwrap();
        contents.push_str("{\"bogus\":\"out-of-band-line-not-via-append\"}\n");
        std::fs::write(&wal_path, contents).unwrap();
    }

    mw.append(sid.clone(), receipt(3)).unwrap();

    // Entry 3 is the last *appended* line. With the bogus injection it's the
    // last line in the file too (we appended after it).
    let wal_path = PathBuf::from(format!("{tmp}/sessions/{}/manifest.wal", sid.0));
    let contents = std::fs::read_to_string(&wal_path).unwrap();
    let last = contents.lines().last().expect("a last WAL line");
    let v: serde_json::Value = serde_json::from_str(last).expect("entry 3 is valid JSON");
    v["prev_hash"]
        .as_str()
        .expect("entry 3 has a prev_hash")
        .to_string()
}

// REGRESSION (perf, NFR-DET-01): append() must compute prev_hash from the
// last line it WROTE for this session (cached), not by re-reading the whole
// WAL each time. Proof without timing: inject an unrelated line into the WAL
// file out of band between appends; the resulting entry-3 prev_hash must be
// IDENTICAL to the clean control (file mutation ignored). The unfixed code
// re-reads the file and chains off the bogus line, so the two diverge -> RED.
// The cached code chains off entry 2 in both -> GREEN.
#[test]
fn append_chains_off_cached_last_line_not_a_full_reread() {
    let control = build_three_and_read_last_prev_hash(
        "/tmp/loom-test-mw-cache-control",
        SessionId("01HZCACHE0CONTROL00000000A".into()),
        false,
    );
    let injected = build_three_and_read_last_prev_hash(
        "/tmp/loom-test-mw-cache-inject",
        SessionId("01HZCACHE0INJECT000000000B".into()),
        true,
    );
    assert_eq!(
        injected, control,
        "append() must chain the next entry off the cached last-written line, \
         not re-read the (here, out-of-band-mutated) WAL file"
    );
}

// DETERMINISM ORACLE (NFR-DET-01): validate() re-derives the chain by reading
// the WAL from DISK, independent of any in-memory cache. If the cached last
// line ever diverged in hashed bytes from what was persisted, the on-disk
// prev_hash links would not match validate()'s disk-derived expectation and
// this would fail. So a warm-cache session that still validates is the concrete
// oracle the council asked for: cache == disk reality.
#[test]
fn warm_cache_appends_still_validate_against_disk() {
    let tmp = "/tmp/loom-test-mw-cache-validate";
    let sid = SessionId("01HZCACHE0VALIDATE0000000C".into());
    std::fs::remove_dir_all(format!("{tmp}/sessions/{}", sid.0)).ok();
    let mw = make_mw(tmp);
    mw.open_manifest(sid.clone(), None).unwrap();
    for id in 1..=12u64 {
        mw.append(
            sid.clone(),
            ManifestEntry::ActionReceipt {
                action_id: id,
                emitted_at_ms: 1_714_000_000_000 + id,
                receipt_canonical_bytes: format!("action{id}").into_bytes(),
                prev_hash: "0".repeat(64),
            },
        )
        .unwrap();
    }
    mw.validate(sid)
        .expect("warm-cache chain must match the on-disk chain validate() re-derives");
}

// COLD-MISS FALLBACK (council: fallback path was uncovered): a SECOND writer
// instance with a cold cache appends onto an existing session's WAL. With no
// cached line it must fall back to reading the file for prev_hash; the chain it
// writes must still validate. This mirrors first-append-after-open, resumed
// sessions, and the benign two-writer startup-sweep pattern.
#[test]
fn cold_cache_second_writer_falls_back_and_chain_validates() {
    let tmp = "/tmp/loom-test-mw-cache-coldmiss";
    let sid = SessionId("01HZCACHE0COLDMISS000000D".into());
    std::fs::remove_dir_all(format!("{tmp}/sessions/{}", sid.0)).ok();

    // Writer 1: header + two receipts (warms ITS OWN cache only).
    let mw1 = make_mw(tmp);
    mw1.open_manifest(sid.clone(), None).unwrap();
    for id in 1..=2u64 {
        mw1.append(
            sid.clone(),
            ManifestEntry::ActionReceipt {
                action_id: id,
                emitted_at_ms: 1_714_000_000_000 + id,
                receipt_canonical_bytes: format!("action{id}").into_bytes(),
                prev_hash: "0".repeat(64),
            },
        )
        .unwrap();
    }

    // Writer 2: brand-new instance, cold cache for this session. Its append must
    // fall back to last_wal_line(file) to chain correctly off writer 1's last line.
    let mw2 = make_mw(tmp);
    mw2.append(
        sid.clone(),
        ManifestEntry::ActionReceipt {
            action_id: 3,
            emitted_at_ms: 1_714_000_000_003,
            receipt_canonical_bytes: b"action3".to_vec(),
            prev_hash: "0".repeat(64),
        },
    )
    .unwrap();

    mw2.validate(sid)
        .expect("cold-cache fallback append must produce a chain that validates");
}
