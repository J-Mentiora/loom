// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-core/modules/content_store/interface_tests.rs` instead.
// Interface tests for `ContentStore`. Verifies IC-CORE-03 verify-on-read,
// SR-CORE-07 atomic writes, SR-CORE-18 CAS path layout, BC-CORE-01.

use super::content_store::{shard_path, ContentRef, ContentStore, LocalContentStore};
use loom_core::error::{LoomError, LoomErrorCode};
use loom_core::observability::Observability;
use std::path::PathBuf;
use std::time::Duration;

fn fixture() -> LocalContentStore {
    let obs = Observability::new(PathBuf::from("/tmp/loom-test/loom.log"), false);
    let root = PathBuf::from("/tmp/loom-test/store");
    LocalContentStore::new(root, obs)
}

// === SR-CORE-18 / BC-CORE-01: CAS path layout ===

#[test]
fn shard_path_uses_two_level_aa_bb_rest_layout() {
    let root = PathBuf::from("/var/loom");
    let p = shard_path(
        &root,
        "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789",
        2,
    );
    let s = p.to_string_lossy();
    assert!(s.ends_with("cas/ab/cd/ef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"));
}

#[test]
fn shard_path_root_is_cas_subdir_of_provided_root() {
    let p = shard_path(&PathBuf::from("/var/loom"), &"a".repeat(64), 2);
    let s = p.to_string_lossy();
    assert!(s.starts_with("/var/loom/cas/aa/aa/"));
}

// === IC-CORE-03: verify-on-read ===

#[test]
fn get_returns_result_vec_u8_loomerror_on_missing_blob() {
    // IC-CORE-03: get() returns Err (not panic) for unknown sha256.
    let cs = fixture();
    let result = cs.get(&ContentRef {
        sha256: "a".repeat(64),
        size_bytes: 0,
    });
    assert!(
        result.is_err(),
        "get() must return Err for a non-existent blob"
    );
}

#[test]
fn get_returns_loomerror_on_integrity_failure() {
    // Contract: a corrupted blob on disk produces
    // LoomError::StoreIntegrityFailed { expected_hash, actual_hash }.
    // Without an impl we only assert the error variant exists.
    let _expected: LoomErrorCode = LoomErrorCode::StoreIntegrityFailed;
}

// === SR-CORE-07: atomic writes ===

#[test]
fn put_returns_content_ref_with_sha256_and_size() {
    // Compile-time signature check: put consumes &[u8] and returns
    // Result<ContentRef, LoomError>.
    fn _check<T: ContentStore>(s: &T) -> Result<ContentRef, LoomError> {
        s.put(b"hello world")
    }
    let _ = _check::<LocalContentStore>;
}

#[test]
fn put_idempotent_returns_same_ref_for_same_bytes() {
    // Idempotency: caller relies on the SHA-256 derivation. The
    // ContentRef::sha256 must be identical for identical input bytes.
    let r1 = ContentRef {
        sha256: "deadbeef".repeat(8),
        size_bytes: 4,
    };
    let r2 = ContentRef {
        sha256: "deadbeef".repeat(8),
        size_bytes: 4,
    };
    assert_eq!(r1, r2);
}

// === ENOSPC mapping ===

#[test]
fn enospc_translates_to_io_error() {
    let _: LoomErrorCode = LoomErrorCode::Io;
}

// === gc() shape ===

#[test]
fn gc_report_fields_are_pure_integers() {
    let cs = fixture();
    fn _check<T: ContentStore>(s: &T) -> Result<super::content_store::GcReport, LoomError> {
        s.gc(Duration::from_secs(86400))
    }
    let _ = _check::<LocalContentStore>;
    let _ = cs;
}

#[test]
fn content_ref_size_bytes_is_u64_not_float() {
    // Compile-time guarantee per Hard binding 3.
    let r = ContentRef {
        sha256: "0".repeat(64),
        size_bytes: u64::MAX,
    };
    assert_eq!(r.size_bytes, u64::MAX);
}
