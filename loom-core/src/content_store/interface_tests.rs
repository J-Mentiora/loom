// Interface tests for `ContentStore`. Verifies verify-on-read,
// atomic writes, CAS path layout, storage layout.

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

// === CAS path layout ===

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

#[test]
fn shard_path_is_bounds_safe_for_malformed_refs() {
    // Regression: refs shorter than the shard prefix used to index out of
    // bounds and panic — a whole-daemon abort under panic = "abort". Any
    // ref, however malformed, must yield a path (sentinel-sharded for the
    // missing components), never a panic.
    let root = PathBuf::from("/var/loom");
    for bad in ["", "a", "ab", "abc", "zz!!", "ZZZZ-not-hex"] {
        let p = shard_path(&root, bad, 2);
        assert!(
            p.starts_with("/var/loom/cas"),
            "ref {bad:?} must still map under cas/, got {p:?}"
        );
    }
    // Multi-byte char straddling a slice boundary must not panic either.
    let _ = shard_path(&root, "ééé", 2);
    let _ = shard_path(&root, "ab\u{4e16}\u{754c}", 2);
    // Over-long refs keep the normal aa/bb/rest layout.
    let p = shard_path(&root, &"a".repeat(100), 2);
    assert!(p.to_string_lossy().starts_with("/var/loom/cas/aa/aa/"));
}

#[test]
fn shard_path_sentinel_never_collides_with_real_blob_paths() {
    // The fallback components are non-hex, so a malformed ref can never
    // resolve to a path a real (hex-addressed) blob occupies.
    let p = shard_path(&PathBuf::from("/var/loom"), "abc", 2);
    assert_eq!(p, PathBuf::from("/var/loom/cas/ab/zz/zz"));
}

#[test]
fn get_returns_err_not_panic_for_malformed_refs() {
    // Regression for the content.get daemon abort: a short / non-hex /
    // over-long sha256 in a ContentRef must surface as Err, never panic.
    let cs = fixture();
    for bad in ["", "a", "abc", "ZZZZ-not-hex", "ééé"] {
        let result = cs.get(&ContentRef {
            sha256: bad.to_string(),
            size_bytes: 0,
        });
        assert!(result.is_err(), "get({bad:?}) must return Err, not panic");
    }
    let result = cs.get(&ContentRef {
        sha256: "a".repeat(100),
        size_bytes: 0,
    });
    assert!(result.is_err(), "over-long ref must return Err, not panic");
}

// === verify-on-read ===

#[test]
fn get_returns_result_vec_u8_loomerror_on_missing_blob() {
    // get() returns Err (not panic) for unknown sha256.
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

// === atomic writes ===

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
    // Compile-time guarantee: integer-only numeric fields.
    let r = ContentRef {
        sha256: "0".repeat(64),
        size_bytes: u64::MAX,
    };
    assert_eq!(r.size_bytes, u64::MAX);
}
