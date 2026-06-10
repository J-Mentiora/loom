// Behavior tests for LocalManifestWriter.
//
// Coverage:
//   - manifest.json public checkpoint: test_manifest_json_checkpoint_has_entries_array
//   - hash chain: test_manifest_hash_chain_validates_intact,
//                 test_manifest_hash_chain_detects_tampering
//   - structured error context: test_validate_corrupt_error_has_structured_context
//   - atomic write: test_manifest_atomic_write_no_tmp_file
//   - JCS compliance (HARD #3): test_manifest_jcs_not_serde_json
//   - append serialization: concurrent_same_session_appends_never_fork_the_chain
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
    let mw = make_mw_at(tmp.path());
    (mw, tmp)
}

// Build a writer rooted at an existing dir — lets two writer instances share
// one on-disk session store (cold-cache fallback test) without each call
// getting its own per-run TempDir.
fn make_mw_at(root: &std::path::Path) -> LocalManifestWriter {
    let obs = Observability::new(root.join("loom.log"), false);
    LocalManifestWriter::new(root.join("sessions"), obs)
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
fn build_three_and_read_last_prev_hash(sid: SessionId, inject_bogus: bool) -> String {
    // Hermetic per-run TempDir (returned so it outlives the WAL reads below) —
    // no shared /tmp, so a stale WAL can never leak across runs.
    let (mw, tmp) = make_mw();
    mw.open_manifest(sid.clone(), None).unwrap();
    let receipt = |id: u64| ManifestEntry::ActionReceipt {
        action_id: id,
        emitted_at_ms: 1_714_000_000_000 + id,
        receipt_canonical_bytes: format!("action{id}").into_bytes(),
        prev_hash: "0".repeat(64), // overwritten by append()
    };
    mw.append(sid.clone(), receipt(1)).unwrap();
    mw.append(sid.clone(), receipt(2)).unwrap();

    let wal_path = tmp
        .path()
        .join("sessions")
        .join(&sid.0)
        .join("manifest.wal");

    if inject_bogus {
        // Append a bogus line directly to the file, bypassing append(). A
        // full-file re-read would see THIS as the last line; the per-session
        // cache must ignore it and chain entry 3 off entry 2.
        let mut contents = std::fs::read_to_string(&wal_path).unwrap();
        contents.push_str("{\"bogus\":\"out-of-band-line-not-via-append\"}\n");
        std::fs::write(&wal_path, contents).unwrap();
    }

    mw.append(sid.clone(), receipt(3)).unwrap();

    // Entry 3 is the last *appended* line. With the bogus injection it's the
    // last line in the file too (we appended after it).
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
    let control =
        build_three_and_read_last_prev_hash(SessionId("01HZCACHE0CONTROL00000000A".into()), false);
    let injected =
        build_three_and_read_last_prev_hash(SessionId("01HZCACHE0INJECT000000000B".into()), true);
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
    let (mw, _tmp) = make_mw();
    let sid = SessionId("01HZCACHE0VALIDATE0000000C".into());
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
    // One shared on-disk store; two writer instances rooted at it so writer 2
    // has a cold cache for this session and must fall back to reading the WAL.
    let tmp = TempDir::new().unwrap();
    let sid = SessionId("01HZCACHE0COLDMISS000000D".into());

    // Writer 1: header + two receipts (warms ITS OWN cache only).
    let mw1 = make_mw_at(tmp.path());
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
    let mw2 = make_mw_at(tmp.path());
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

// === per-session append serialization (chain-fork regression) ===

// REGRESSION (NFR-DET-01): append() used to be an unlocked read-prev-hash ->
// writeln+fsync sequence, so concurrent same-session appends could both read
// the same predecessor and fork the chain (two WAL lines sharing one
// prev_hash; validate() -> ManifestCorrupt). In production receipt appends
// run on detached tokio tasks while audits/terminals append from other
// tasks. Fire many appends in parallel and assert the chain stayed linear:
// validate() re-derives every prev_hash from the on-disk predecessor, and
// the manual scan below pins no-fork (all prev_hash unique) and no torn
// lines (every line parses).
#[test]
fn concurrent_same_session_appends_never_fork_the_chain() {
    let (mw, tmp) = make_mw();
    let mw = std::sync::Arc::new(mw);
    let sid = SessionId("01HZCONCURRENT0APPENDS000E".into());
    mw.open_manifest(sid.clone(), None).unwrap();

    const THREADS: u64 = 16;
    const APPENDS_PER_THREAD: u64 = 4;
    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            let mw = mw.clone();
            let sid = sid.clone();
            std::thread::spawn(move || {
                for i in 0..APPENDS_PER_THREAD {
                    let id = t * APPENDS_PER_THREAD + i + 1;
                    mw.append(
                        sid.clone(),
                        ManifestEntry::ActionReceipt {
                            action_id: id,
                            emitted_at_ms: 1_714_000_000_000 + id,
                            receipt_canonical_bytes: format!("action{id}").into_bytes(),
                            prev_hash: "0".repeat(64), // overwritten by append()
                        },
                    )
                    .unwrap();
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    // Oracle 1: validate() re-derives each line's prev_hash from the hash of
    // the previous on-disk line — a forked or interleaved chain fails here.
    mw.validate(sid.clone())
        .expect("concurrent same-session appends must produce a linear chain");

    // Oracle 2: linearity, directly. Exactly header + N lines, every line
    // parses (no torn writes), and no two entries share a prev_hash (a
    // duplicate means two appends chained off the same predecessor — a fork
    // validate() would also flag, pinned here without the hash round-trip).
    let wal_path = tmp
        .path()
        .join("sessions")
        .join(&sid.0)
        .join("manifest.wal");
    let contents = std::fs::read_to_string(&wal_path).unwrap();
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(
        lines.len() as u64,
        THREADS * APPENDS_PER_THREAD + 1,
        "header + one WAL line per append"
    );
    let mut seen_prev_hashes = std::collections::HashSet::new();
    for line in &lines[1..] {
        let v: serde_json::Value = serde_json::from_str(line).expect("no torn WAL lines");
        let prev = v["prev_hash"].as_str().expect("entry has prev_hash");
        assert!(
            seen_prev_hashes.insert(prev.to_string()),
            "two entries chained off the same predecessor (forked chain): {prev}"
        );
    }
}
