// ContentStore — content-addressed blob store with verify-on-read.
//
// # Contract semantics
// - Atomic writes via `O_TMPFILE` + `linkat`.
// - CAS path layout: `~/.../store/cas/<aa>/<bb>/<rest>`.
// - Every `get()` re-hashes and emits `StoreIntegrityFailed` on mismatch.
//   NO BYPASS.
// - GC traverses all session manifests; not a hot path.
// - Errors at boundary translate to `LoomErrorCode` via `From<io::Error>`.
//   `ENOSPC` becomes `StoreFullNoEvictable`.
//
// Crate type generation note: `ContentRef` is a WIT-derived type.
// The struct shown here is the shape `wit-bindgen` produces.

use loom_core::error::LoomError;
use loom_core::observability::Observability;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Content-addressed reference. SHA-256 hex (lowercase, 64 chars).
/// Defined in `wit/loom-surface.wit`; this is the wit-bindgen shape.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentRef {
    pub sha256: String,
    pub size_bytes: u64,
}

/// GC report. Pure integer fields per BC HARD #3.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GcReport {
    pub blobs_scanned: u64,
    pub blobs_collected: u64,
    pub bytes_freed: u64,
}

/// Concrete CAS implementation. `loom-core` exposes only this one impl;
/// the trait `ContentStore` is implemented by it.
pub struct LocalContentStore {
    pub(crate) root: PathBuf,
    pub(crate) shard_depth: u8,
    pub(crate) obs: Arc<Observability>,
    /// Optional storage cap (bytes). When `Some(limit)`, `put` runs GC
    /// before writing if the store would exceed the limit.
    pub(crate) max_bytes: Option<u64>,
    /// TTL used by the auto-GC triggered by `max_bytes` enforcement.
    pub(crate) default_gc_ttl: Duration,
}

impl LocalContentStore {
    /// Construct a new local CAS rooted at `root`. The root must exist;
    /// per-shard directories are created lazily on first `put`.
    pub fn new(root: PathBuf, obs: Arc<Observability>) -> Self {
        Self {
            root,
            shard_depth: 2,
            obs,
            max_bytes: None,
            default_gc_ttl: Duration::from_secs(86400 * 30),
        }
    }

    /// Construct with storage limit and GC TTL (for `--store-max-bytes`).
    pub fn new_with_config(
        root: PathBuf,
        obs: Arc<Observability>,
        max_bytes: Option<u64>,
        default_gc_ttl: Duration,
    ) -> Self {
        Self {
            root,
            shard_depth: 2,
            obs,
            max_bytes,
            default_gc_ttl,
        }
    }
}

/// Public trait surface (per `loom-core_contract.md`).
pub trait ContentStore: Send + Sync {
    /// Idempotent atomic write. Returns ContentRef.
    /// Pre: bytes.len() ≤ MAX_BLOB_BYTES.
    /// Post: blob persisted at `cas/<aa>/<bb>/<rest>`.
    /// Errors: StoreFullNoEvictable (ENOSPC), generic IoError.
    fn put(&self, bytes: &[u8]) -> Result<ContentRef, LoomError>;

    /// Read with mandatory hash re-verification.
    /// Errors: StoreIntegrityFailed { expected_hash, actual_hash };
    ///         LoomError on missing blob.
    fn get(&self, r: &ContentRef) -> Result<Vec<u8>, LoomError>;

    /// GC blobs unreferenced by any session manifest older than `ttl`.
    fn gc(&self, ttl: Duration) -> Result<GcReport, LoomError>;
}

/// Compute the on-disk path for a blob given its sha256 hex and shard depth.
/// Pure helper; no I/O. Useful for tests and StartupManager sweep.
pub fn shard_path(root: &std::path::Path, sha256_hex: &str, depth: u8) -> PathBuf {
    let mut p = root.to_path_buf();
    p.push("cas");
    let bytes = sha256_hex.as_bytes();
    for i in 0..depth as usize {
        let start = i * 2;
        let end = start + 2;
        p.push(std::str::from_utf8(&bytes[start..end]).unwrap_or("zz"));
    }
    p.push(&sha256_hex[(depth as usize) * 2..]);
    p
}
