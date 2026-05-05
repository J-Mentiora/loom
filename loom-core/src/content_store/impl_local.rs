//! `LocalContentStore` — real implementation of the `ContentStore` trait.
//!
//! Atomicity: `NamedTempFile::new_in(shard_dir)` + `persist(target)` is an
//! atomic `rename(2)` on the same filesystem; safe on macOS and Linux.
//! CAS identity guarantees that even if two concurrent `put`s race, both
//! write identical bytes and the last rename wins without data loss.

use super::content_store::{shard_path, ContentRef, ContentStore, GcReport, LocalContentStore};
use crate::error::{LoomError, LoomErrorCode};
use ring::digest::{digest, SHA256};
use serde_json::json;
use std::fs;
use std::io::ErrorKind;
use std::time::{Duration, SystemTime};
use tempfile::NamedTempFile;
use walkdir::WalkDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn sha256_hex(data: &[u8]) -> String {
    let d = digest(&SHA256, data);
    d.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Compute the total byte-size of all blobs under `cas_root`.
fn cas_total_bytes(cas_root: &std::path::Path) -> u64 {
    if !cas_root.exists() {
        return 0;
    }
    WalkDir::new(cas_root)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok().map(|m| m.len()))
        .sum()
}

/// Reconstruct the 64-char sha256 hex from a CAS shard path.
/// `cas/<aa>/<bb>/<rest>` → `aabbrest`
fn sha256_from_shard_path(path: &std::path::Path) -> Option<String> {
    let file_name = path.file_name()?.to_str()?;
    let bb = path.parent()?.file_name()?.to_str()?;
    let aa = path.parent()?.parent()?.file_name()?.to_str()?;
    // Verify these look like shard hex components (2 chars each for depth=2)
    if aa.len() != 2 || bb.len() != 2 {
        return None;
    }
    let sha256 = format!("{aa}{bb}{file_name}");
    if sha256.len() == 64 && sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(sha256)
    } else {
        None
    }
}

/// Walk all manifest files (both .jsonl and .wal) in `sessions_root` and
/// collect every 64-char lowercase hex string referenced by an action
/// receipt. These are conservatively treated as referenced CAS blobs
/// and skipped by GC even when their mtime would otherwise put them
/// past the TTL cutoff.
///
/// Two layers are walked:
///
/// 1. The outer manifest entry — catches `prev_hash` (chain hash, not
///    a CAS blob, but harmless to over-protect) plus any future
///    string-typed hash fields a receipt-shape change might introduce.
///
/// 2. The decoded `receipt_canonical_bytes` byte-array on `ActionReceipt`
///    entries. The actual CAS references live HERE (`dom_snapshot_hash`,
///    `screenshot_after_hash`, `content_refs[].sha256`, etc.) because
///    the receipt is stored as canonical-JSON bytes inside the WAL line,
///    not unrolled into the WAL JSON itself. Without this decode pass
///    GC happily deleted blobs that active sessions still referenced
///    — a data-loss bug surfaced by Phase 8 round-23 testing
///    (`gc --ttl 0` after a `web.navigate` deleted the navigate's
///    DOM + screenshot blobs even though the session's manifest still
///    pointed at them).
fn collect_referenced_blobs(sessions_root: &std::path::Path) -> std::collections::HashSet<String> {
    let mut referenced = std::collections::HashSet::new();
    if !sessions_root.exists() {
        return referenced;
    }
    for entry in WalkDir::new(sessions_root).into_iter().flatten() {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !matches!(ext, "jsonl" | "wal") {
            continue;
        }
        let content = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        for line in content.lines() {
            let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            // Layer 1: outer manifest entry (catches `prev_hash` and
            // any string-typed hash field a future receipt change adds).
            collect_hex64_strings(&val, &mut referenced);
            // Layer 2: decoded receipt_canonical_bytes.
            if let Some(arr) = val
                .get("receipt_canonical_bytes")
                .and_then(|v| v.as_array())
            {
                let bytes: Vec<u8> = arr
                    .iter()
                    .filter_map(|v| v.as_u64())
                    .filter_map(|n| u8::try_from(n).ok())
                    .collect();
                if let Ok(text) = std::str::from_utf8(&bytes) {
                    if let Ok(receipt_val) = serde_json::from_str::<serde_json::Value>(text) {
                        collect_hex64_strings(&receipt_val, &mut referenced);
                    }
                }
            }
        }
    }
    referenced
}

fn collect_hex64_strings(val: &serde_json::Value, out: &mut std::collections::HashSet<String>) {
    match val {
        serde_json::Value::String(s)
            if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()) =>
        {
            out.insert(s.clone());
        }
        serde_json::Value::Object(map) => {
            for v in map.values() {
                collect_hex64_strings(v, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                collect_hex64_strings(v, out);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// ContentStore impl
// ---------------------------------------------------------------------------

impl ContentStore for LocalContentStore {
    fn put(&self, bytes: &[u8]) -> Result<ContentRef, LoomError> {
        let sha256_hex = sha256_hex(bytes);
        let size_bytes = bytes.len() as u64;
        let target = shard_path(&self.root, &sha256_hex, self.shard_depth);

        // Idempotent: if the blob already exists, skip disk I/O.
        if target.exists() {
            return Ok(ContentRef {
                sha256: sha256_hex,
                size_bytes,
            });
        }

        // Auto-GC check: if max_bytes is set and we'd exceed it, run GC first.
        if let Some(limit) = self.max_bytes {
            let cas_root = self.root.join("cas");
            let current = cas_total_bytes(&cas_root);
            if current + size_bytes > limit {
                let report = self.gc(self.default_gc_ttl)?;
                // Re-check after GC.
                let after_gc = cas_total_bytes(&cas_root);
                if after_gc + size_bytes > limit && report.blobs_collected == 0 {
                    return Err(LoomError::new(
                        LoomErrorCode::StoreFullNoEvictable,
                        format!("store at capacity ({after_gc} bytes used, limit {limit})"),
                    ));
                }
            }
        }

        // Create shard directory lazily.
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }

        // Atomic write via temp file + rename (cross-platform safe).
        let tmp = NamedTempFile::new_in(target.parent().unwrap_or(&self.root))
            .map_err(LoomError::from)?;
        fs::write(tmp.path(), bytes)?;
        tmp.persist(&target).map_err(|e| LoomError::from(e.error))?;

        Ok(ContentRef {
            sha256: sha256_hex,
            size_bytes,
        })
    }

    fn get(&self, r: &ContentRef) -> Result<Vec<u8>, LoomError> {
        let target = shard_path(&self.root, &r.sha256, self.shard_depth);

        let bytes = match fs::read(&target) {
            Ok(b) => b,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Err(LoomError::new(
                    LoomErrorCode::StoreNotFound,
                    format!("blob not found: {}", r.sha256),
                ));
            }
            Err(e) => return Err(LoomError::from(e)),
        };

        // Mandatory integrity re-verification (IC-CORE-03).
        let actual = sha256_hex(&bytes);
        if actual != r.sha256 {
            return Err(LoomError::new(
                LoomErrorCode::StoreIntegrityFailed,
                format!("hash mismatch for blob {}", r.sha256),
            )
            .with_context(json!({
                "expected_hash": r.sha256,
                "actual_hash": actual,
            })));
        }

        Ok(bytes)
    }

    fn gc(&self, ttl: Duration) -> Result<GcReport, LoomError> {
        let cas_root = self.root.join("cas");
        let sessions_root = self
            .root
            .parent()
            .map(|p| p.join("sessions"))
            .unwrap_or_else(|| self.root.join("sessions"));

        let referenced = collect_referenced_blobs(&sessions_root);
        let cutoff = SystemTime::now()
            .checked_sub(ttl)
            .unwrap_or(SystemTime::UNIX_EPOCH);

        let mut blobs_scanned: u64 = 0;
        let mut blobs_collected: u64 = 0;
        let mut bytes_freed: u64 = 0;

        if !cas_root.exists() {
            return Ok(GcReport {
                blobs_scanned,
                blobs_collected,
                bytes_freed,
            });
        }

        for entry in WalkDir::new(&cas_root).into_iter().flatten() {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let Some(sha256) = sha256_from_shard_path(path) else {
                continue;
            };
            blobs_scanned += 1;

            if referenced.contains(&sha256) {
                continue;
            }

            let mtime = match entry.metadata().ok().and_then(|m| m.modified().ok()) {
                Some(t) => t,
                None => continue,
            };

            if mtime <= cutoff {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                if fs::remove_file(path).is_ok() {
                    blobs_collected += 1;
                    bytes_freed += size;
                }
            }
        }

        Ok(GcReport {
            blobs_scanned,
            blobs_collected,
            bytes_freed,
        })
    }
}
