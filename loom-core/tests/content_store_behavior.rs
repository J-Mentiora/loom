//! AC-driven integration tests for `ContentStore` / `LocalContentStore`.
//!
//! AC-CORE-02.1 — write idempotence
//! AC-CORE-02.2 — integrity-on-read
//! AC-STORE-01.1 — gc removes unreferenced blobs older than TTL
//! AC-STORE-01.2 — max_bytes triggers auto-GC before write

use loom_core::content_store::{ContentRef, ContentStore, LocalContentStore};
use loom_core::error::LoomErrorCode;
use loom_core::observability::Observability;
use std::fs;
use std::time::Duration;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Test fixture
// ---------------------------------------------------------------------------

fn fixture() -> (LocalContentStore, TempDir) {
    let tmp = TempDir::new().unwrap();
    let store_root = tmp.path().join("store");
    fs::create_dir_all(&store_root).unwrap();
    // Also create sessions/ sibling for gc tests.
    fs::create_dir_all(tmp.path().join("sessions")).unwrap();
    let obs = Observability::new(store_root.join("loom.log"), false);
    let cs = LocalContentStore::new(store_root, obs);
    (cs, tmp)
}

#[allow(dead_code)]
fn fixture_with_limit(max_bytes: u64, ttl: Duration) -> (LocalContentStore, TempDir) {
    let tmp = TempDir::new().unwrap();
    let store_root = tmp.path().join("store");
    fs::create_dir_all(&store_root).unwrap();
    fs::create_dir_all(tmp.path().join("sessions")).unwrap();
    let obs = Observability::new(store_root.join("loom.log"), false);
    let cs = LocalContentStore::new_with_config(store_root, obs, Some(max_bytes), ttl);
    (cs, tmp)
}

// ---------------------------------------------------------------------------
// AC-CORE-02.1 — Content-addressed write idempotence
// ---------------------------------------------------------------------------

#[test]
fn ac_core_02_1_put_same_bytes_twice_returns_same_ref() {
    let (cs, _tmp) = fixture();
    let r1 = cs.put(b"hello loom").unwrap();
    let r2 = cs.put(b"hello loom").unwrap();
    assert_eq!(r1.sha256, r2.sha256);
    assert_eq!(r1.size_bytes, r2.size_bytes);
}

#[test]
fn ac_core_02_1_put_idempotent_single_blob_on_disk() {
    let (cs, tmp) = fixture();
    let r = cs.put(b"hello loom").unwrap();
    cs.put(b"hello loom").unwrap(); // second write
    // Count files under cas/
    let cas_root = tmp.path().join("store").join("cas");
    let count = walkdir_files(&cas_root);
    assert_eq!(count, 1, "expected exactly one blob on disk after two identical puts");
    // sha256 must be the actual SHA-256 of the bytes
    let expected = sha256_hex(b"hello loom");
    assert_eq!(r.sha256, expected);
}

#[test]
fn ac_core_02_1_put_content_ref_size_bytes_matches_input() {
    let (cs, _tmp) = fixture();
    let data = b"size check bytes";
    let r = cs.put(data).unwrap();
    assert_eq!(r.size_bytes, data.len() as u64);
}

#[test]
fn ac_core_02_1_different_bytes_produce_different_refs() {
    let (cs, _tmp) = fixture();
    let r1 = cs.put(b"data one").unwrap();
    let r2 = cs.put(b"data two").unwrap();
    assert_ne!(r1.sha256, r2.sha256);
}

// ---------------------------------------------------------------------------
// AC-CORE-02.2 — Integrity-on-read
// ---------------------------------------------------------------------------

#[test]
fn ac_core_02_2_get_returns_original_bytes() {
    let (cs, _tmp) = fixture();
    let data = b"round trip test data";
    let r = cs.put(data).unwrap();
    let got = cs.get(&r).unwrap();
    assert_eq!(got, data);
}

#[test]
fn ac_core_02_2_get_corrupted_blob_returns_store_integrity_failed() {
    let (cs, tmp) = fixture();
    let r = cs.put(b"original content").unwrap();
    // Corrupt the blob on disk.
    let blob_path = loom_core::content_store::shard_path(
        &tmp.path().join("store"),
        &r.sha256,
        2,
    );
    fs::write(&blob_path, b"corrupted!").unwrap();
    let err = cs.get(&r).unwrap_err();
    assert_eq!(err.code, LoomErrorCode::StoreIntegrityFailed);
}

#[test]
fn ac_core_02_2_integrity_error_context_has_expected_and_actual_hash() {
    let (cs, tmp) = fixture();
    let r = cs.put(b"integrity test").unwrap();
    let blob_path = loom_core::content_store::shard_path(
        &tmp.path().join("store"),
        &r.sha256,
        2,
    );
    fs::write(&blob_path, b"tampered bytes here").unwrap();
    let err = cs.get(&r).unwrap_err();
    assert_eq!(err.code, LoomErrorCode::StoreIntegrityFailed);
    let ctx = err.context.as_ref().expect("integrity error must carry context");
    assert_eq!(ctx["expected_hash"], r.sha256.as_str());
    let actual = ctx["actual_hash"].as_str().unwrap();
    assert_ne!(actual, r.sha256, "actual_hash must differ from expected");
    assert_eq!(actual.len(), 64, "actual_hash must be a 64-char hex string");
}

#[test]
fn ac_core_02_2_get_missing_blob_returns_store_not_found() {
    let (cs, _tmp) = fixture();
    let phantom = ContentRef {
        sha256: "a".repeat(64),
        size_bytes: 1,
    };
    let err = cs.get(&phantom).unwrap_err();
    assert_eq!(err.code, LoomErrorCode::StoreNotFound);
}

// ---------------------------------------------------------------------------
// AC-STORE-01.1 — gc removes unreferenced blobs older than TTL
// ---------------------------------------------------------------------------

#[test]
fn ac_store_01_1_gc_removes_unreferenced_old_blobs() {
    let (cs, tmp) = fixture();
    // Write 5 blobs.
    let mut refs: Vec<ContentRef> = (0..5)
        .map(|i| cs.put(format!("blob-{i}").as_bytes()).unwrap())
        .collect();
    // Reference the first 2 in a fake manifest.
    let sessions = tmp.path().join("sessions").join("01AAAAAAAAAAAAAAAAAAAAAAAAA");
    fs::create_dir_all(&sessions).unwrap();
    let manifest_content = refs[..2]
        .iter()
        .map(|r| format!("{{\"kind\":\"action_receipt\",\"receipt_canonical_bytes\":\"{}\"}}", r.sha256))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(sessions.join("manifest.jsonl"), manifest_content).unwrap();

    // Back-date the 3 unreferenced blobs to exceed TTL.
    let ttl = Duration::from_secs(3600);
    let old_mtime = std::time::SystemTime::now() - ttl - Duration::from_secs(1);
    for r in &refs[2..] {
        let p = loom_core::content_store::shard_path(&tmp.path().join("store"), &r.sha256, 2);
        let ft = filetime::FileTime::from_system_time(old_mtime);
        filetime::set_file_mtime(&p, ft).unwrap();
    }

    let report = cs.gc(ttl).unwrap();
    assert_eq!(report.blobs_collected, 3, "3 unreferenced old blobs should be collected");
    assert_eq!(report.blobs_scanned, 5);
    assert!(report.bytes_freed > 0);

    // Referenced blobs still exist.
    for r in &refs[..2] {
        assert!(cs.get(r).is_ok(), "referenced blob must survive GC");
    }
    // Unreferenced blobs are gone.
    for r in &refs[2..] {
        let err = cs.get(r).unwrap_err();
        assert_eq!(err.code, LoomErrorCode::StoreNotFound);
    }
    let _ = refs.drain(..); // suppress unused warning
}

#[test]
fn ac_store_01_1_gc_retains_blobs_within_ttl() {
    let (cs, _tmp) = fixture();
    let r = cs.put(b"fresh blob").unwrap();
    // Use a very long TTL so the just-written blob is within it.
    let report = cs.gc(Duration::from_secs(86400 * 365)).unwrap();
    assert_eq!(report.blobs_collected, 0, "fresh blob must not be collected");
    assert!(cs.get(&r).is_ok());
}

// Phase 8 round-23 regression: GC must protect blobs referenced by a
// real-shape WAL entry, where receipt_canonical_bytes is a byte ARRAY
// (not a string). The pre-fix `collect_referenced_blobs` only scanned
// string values, so it missed every CAS reference baked into a real
// receipt and happily deleted in-use blobs.
#[test]
fn gc_protects_blobs_referenced_inside_receipt_canonical_bytes() {
    let (cs, tmp) = fixture();

    // Write 3 blobs. The first two will be referenced by a realistic
    // ActionReceipt WAL entry; the third will be unreferenced.
    let dom_blob = cs.put(b"dom snapshot bytes").unwrap();
    let screenshot_blob = cs.put(b"screenshot png bytes").unwrap();
    let unreferenced = cs.put(b"orphan bytes").unwrap();

    // Build a receipt whose canonical-JSON form references the first
    // two blobs by their SHA-256 hex (mirrors the real navigate
    // receipt shape: dom_snapshot_hash + screenshot_after_hash).
    let receipt_json = serde_json::json!({
        "code": "web_action_completed",
        "dom_snapshot_hash": dom_blob.sha256,
        "screenshot_after_hash": screenshot_blob.sha256,
        "status": "ok",
    });
    let receipt_bytes = serde_jcs::to_vec(&receipt_json).unwrap();

    // Build the WAL line in the production shape: kind=action_receipt,
    // receipt_canonical_bytes is a JSON byte array.
    let wal_entry = serde_json::json!({
        "kind": "action_receipt",
        "action_id": 0,
        "emitted_at_ms": 0,
        "prev_hash": "0".repeat(64),
        "receipt_canonical_bytes": receipt_bytes,
    });
    let sessions = tmp.path().join("sessions").join("01TESTSESSION00000000000001");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("manifest.wal"),
        serde_json::to_string(&wal_entry).unwrap(),
    )
    .unwrap();

    // Back-date all 3 blobs past the cutoff so TTL alone doesn't save them.
    let ttl = Duration::from_secs(3600);
    let old_mtime = std::time::SystemTime::now() - ttl - Duration::from_secs(1);
    for r in [&dom_blob, &screenshot_blob, &unreferenced] {
        let p = loom_core::content_store::shard_path(&tmp.path().join("store"), &r.sha256, 2);
        let ft = filetime::FileTime::from_system_time(old_mtime);
        filetime::set_file_mtime(&p, ft).unwrap();
    }

    let report = cs.gc(ttl).unwrap();

    // The two blobs baked into receipt_canonical_bytes MUST survive,
    // even though their mtime is past the cutoff.
    assert!(
        cs.get(&dom_blob).is_ok(),
        "DOM blob referenced inside receipt_canonical_bytes must survive GC"
    );
    assert!(
        cs.get(&screenshot_blob).is_ok(),
        "screenshot blob referenced inside receipt_canonical_bytes must survive GC"
    );
    // The unreferenced blob is fair game.
    let err = cs.get(&unreferenced).unwrap_err();
    assert_eq!(err.code, LoomErrorCode::StoreNotFound);
    assert_eq!(report.blobs_collected, 1);
    assert_eq!(report.blobs_scanned, 3);
}

#[test]
fn ac_store_01_1_gc_report_counts_are_correct() {
    let (cs, _tmp) = fixture();
    // No blobs — gc should return zeros.
    let report = cs.gc(Duration::from_secs(1)).unwrap();
    assert_eq!(report.blobs_scanned, 0);
    assert_eq!(report.blobs_collected, 0);
    assert_eq!(report.bytes_freed, 0);
}

// ---------------------------------------------------------------------------
// AC-STORE-01.2 — --store-max-bytes triggers auto-GC before write
// ---------------------------------------------------------------------------

#[test]
fn ac_store_01_2_put_at_limit_triggers_gc_and_succeeds() {
    // Put one blob, then set max_bytes to just above zero so the next write
    // triggers GC. After GC (TTL=0 so the first blob is evictable), write succeeds.
    let tmp = TempDir::new().unwrap();
    let store_root = tmp.path().join("store");
    fs::create_dir_all(&store_root).unwrap();
    fs::create_dir_all(tmp.path().join("sessions")).unwrap();

    let obs = Observability::new(store_root.join("loom.log"), false);
    // Phase 1: write first blob with no limit.
    let cs_prep = LocalContentStore::new(store_root.clone(), obs.clone());
    let r1 = cs_prep.put(b"evictable blob").unwrap();
    let blob_path = loom_core::content_store::shard_path(&store_root, &r1.sha256, 2);
    let blob_size = blob_path.metadata().unwrap().len();

    // Back-date the blob so it exceeds TTL=0.
    let ft = filetime::FileTime::from_unix_time(0, 0);
    filetime::set_file_mtime(&blob_path, ft).unwrap();

    // Phase 2: re-open with max_bytes = blob_size (exactly at limit).
    let cs = LocalContentStore::new_with_config(
        store_root.clone(),
        obs,
        Some(blob_size),
        Duration::from_secs(0), // TTL=0: everything is evictable
    );
    // Writing a NEW blob should trigger auto-GC (evicts r1) then succeed.
    let result = cs.put(b"new blob after gc");
    assert!(result.is_ok(), "put after auto-GC should succeed; got: {result:?}");
}

#[test]
fn ac_store_01_2_put_at_limit_with_no_evictable_returns_store_full() {
    let tmp = TempDir::new().unwrap();
    let store_root = tmp.path().join("store");
    fs::create_dir_all(&store_root).unwrap();
    fs::create_dir_all(tmp.path().join("sessions")).unwrap();

    let obs = Observability::new(store_root.join("loom.log"), false);
    // Write a blob without limit first.
    let cs_prep = LocalContentStore::new(store_root.clone(), obs.clone());
    let r1 = cs_prep.put(b"referenced blob stays").unwrap();
    let blob_size = loom_core::content_store::shard_path(&store_root, &r1.sha256, 2)
        .metadata().unwrap().len();

    // Reference the blob in a manifest so it won't be collected.
    let sessions = tmp.path().join("sessions").join("01BBBBBBBBBBBBBBBBBBBBBBBBB");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("manifest.jsonl"),
        format!("{{\"sha256\":\"{}\"}}", r1.sha256),
    ).unwrap();

    // Re-open with max_bytes exactly at existing size, TTL very long.
    let cs = LocalContentStore::new_with_config(
        store_root,
        obs,
        Some(blob_size),
        Duration::from_secs(86400 * 365), // nothing old enough to collect
    );
    // Writing new blob: GC runs but collects nothing → StoreFullNoEvictable.
    let err = cs.put(b"this should fail").unwrap_err();
    assert_eq!(err.code, LoomErrorCode::StoreFullNoEvictable);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn walkdir_files(dir: &std::path::Path) -> usize {
    if !dir.exists() {
        return 0;
    }
    let mut count = 0;
    for entry in walkdir::WalkDir::new(dir).into_iter().flatten() {
        if entry.file_type().is_file() {
            count += 1;
        }
    }
    count
}

fn sha256_hex(data: &[u8]) -> String {
    use ring::digest::{digest, SHA256};
    let d = digest(&SHA256, data);
    d.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}
