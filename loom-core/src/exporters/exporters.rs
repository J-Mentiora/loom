// Exporter — produces JSON manifest, gzipped tarball, and HAR 1.2
// artifacts from closed Loom sessions.
//
// # Contract semantics
// - Reads `sessions_root/<session_id>/manifest.wal` (JSONL, JCS-written).
// - Blob retrieval via `ContentStore::get` (respects content-integrity checks).
// - `export_tarball`: gzip + tar with `manifest.json` + `cas/<sha256>` blobs.
// - `export_har`: HAR 1.2 JSON, one entry per `ActionReceipt` in the WAL.
// - `export_json`: JSON manifest with `manifest` + `content_blob_index` keys.

use crate::content_store::ContentStore;
use crate::error::{LoomError, LoomErrorCode};
use crate::manifest_writer::ManifestEntry;
use crate::receipt_builder::receipt_builder::{NetworkEvent, ReceiptPayload};
use flate2::write::GzEncoder;
use flate2::Compression;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

/// Produces export artifacts from closed sessions.
pub struct Exporter {
    sessions_root: PathBuf,
    content_store: Arc<dyn ContentStore>,
}

impl Exporter {
    pub fn new(sessions_root: PathBuf, content_store: Arc<dyn ContentStore>) -> Self {
        Self {
            sessions_root,
            content_store,
        }
    }

    /// Export session as JSON manifest with content blob index.
    /// Returns `{"manifest": {...}, "content_blob_index": [...]}`.
    pub fn export_json(&self, session_id: &str) -> Result<Vec<u8>, LoomError> {
        let wal_path = self.sessions_root.join(session_id).join("manifest.wal");
        let content = std::fs::read_to_string(&wal_path)?;

        let mut actions = Vec::new();
        let mut blob_hashes: BTreeSet<String> = BTreeSet::new();
        let mut started_at_ms: u64 = 0;

        for line in content.lines() {
            if line.is_empty() {
                continue;
            }
            let entry: ManifestEntry = serde_json::from_str(line)
                .map_err(|e| LoomError::new(LoomErrorCode::ManifestCorrupt, e.to_string()))?;
            match entry {
                ManifestEntry::Header {
                    started_at_ms: t, ..
                } => {
                    started_at_ms = t;
                }
                ManifestEntry::ActionReceipt {
                    action_id,
                    emitted_at_ms,
                    receipt_canonical_bytes,
                    ..
                } => {
                    actions.push(serde_json::json!({
                        "action_id": action_id,
                        "emitted_at_ms": emitted_at_ms,
                    }));
                    if let Ok(val) =
                        serde_json::from_slice::<serde_json::Value>(&receipt_canonical_bytes)
                    {
                        collect_hex64_strings(&val, &mut blob_hashes);
                    }
                }
                _ => {}
            }
        }

        let content_blob_index: Vec<String> = blob_hashes.into_iter().collect();
        let doc = serde_json::json!({
            "manifest": {
                "session_id": session_id,
                "started_at_ms": started_at_ms,
                "actions": actions,
            },
            "content_blob_index": content_blob_index,
        });

        serde_json::to_vec(&doc).map_err(|e| LoomError::internal(e.to_string()))
    }

    /// Export session as gzip-compressed tar archive.
    /// Contains `manifest.json` + `cas/<sha256>` blob entries.
    pub fn export_tarball(&self, session_id: &str) -> Result<Vec<u8>, LoomError> {
        use crate::content_store::ContentRef;

        let session_dir = self.sessions_root.join(session_id);
        let wal_path = session_dir.join("manifest.wal");
        let manifest_json_path = session_dir.join("manifest.json");

        let content = std::fs::read_to_string(&wal_path)?;
        let mut blob_hashes: BTreeSet<String> = BTreeSet::new();

        for line in content.lines() {
            if line.is_empty() {
                continue;
            }
            let entry: ManifestEntry = serde_json::from_str(line)
                .map_err(|e| LoomError::new(LoomErrorCode::ManifestCorrupt, e.to_string()))?;
            if let ManifestEntry::ActionReceipt {
                receipt_canonical_bytes,
                ..
            } = entry
            {
                if let Ok(val) =
                    serde_json::from_slice::<serde_json::Value>(&receipt_canonical_bytes)
                {
                    collect_hex64_strings(&val, &mut blob_hashes);
                }
            }
        }

        let manifest_bytes = if manifest_json_path.exists() {
            std::fs::read(&manifest_json_path)?
        } else {
            b"{}".to_vec()
        };

        let output: Vec<u8> = Vec::new();
        let gz = GzEncoder::new(output, Compression::default());
        let mut tar_builder = tar::Builder::new(gz);

        append_bytes_to_tar(&mut tar_builder, "manifest.json", &manifest_bytes)?;

        for hash in &blob_hashes {
            let blob = self.content_store.get(&ContentRef {
                sha256: hash.clone(),
                size_bytes: 0,
            })?;
            append_bytes_to_tar(&mut tar_builder, &format!("cas/{hash}"), &blob)?;
        }

        let gz_encoder = tar_builder.into_inner()?;
        gz_encoder.finish().map_err(LoomError::from)
    }

    /// Export session as HAR 1.2 JSON.
    /// One HAR entry per `NetworkEvent` recorded by the shim during a
    /// navigate; non-navigate receipts (click, evaluate, error) and
    /// navigate receipts with empty `network_events` contribute no
    /// entries.
    pub fn export_har(&self, session_id: &str) -> Result<Vec<u8>, LoomError> {
        let wal_path = self.sessions_root.join(session_id).join("manifest.wal");
        let content = std::fs::read_to_string(&wal_path)?;

        let mut entries = Vec::new();

        for line in content.lines() {
            if line.is_empty() {
                continue;
            }
            let entry: ManifestEntry = serde_json::from_str(line)
                .map_err(|e| LoomError::new(LoomErrorCode::ManifestCorrupt, e.to_string()))?;
            if let ManifestEntry::ActionReceipt {
                emitted_at_ms,
                receipt_canonical_bytes,
                ..
            } = entry
            {
                // Non-navigate receipts (click/evaluate/error) deserialize
                // fine but carry no network_events — produce no entries.
                if let Ok(payload) =
                    serde_json::from_slice::<ReceiptPayload>(&receipt_canonical_bytes)
                {
                    for ev in &payload.network_events {
                        entries.push(make_har_entry(emitted_at_ms, ev));
                    }
                }
            }
        }

        let har = serde_json::json!({
            "log": {
                "version": "1.2",
                "creator": {
                    "name": "loom",
                    "version": "0.1.0",
                },
                "entries": entries,
            }
        });

        serde_json::to_vec(&har).map_err(|e| LoomError::internal(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

fn append_bytes_to_tar<W: std::io::Write>(
    builder: &mut tar::Builder<W>,
    path: &str,
    data: &[u8],
) -> Result<(), LoomError> {
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, path, std::io::Cursor::new(data))
        .map_err(LoomError::from)
}

fn make_har_entry(emitted_at_ms: u64, ev: &NetworkEvent) -> serde_json::Value {
    // timing_ticks is microseconds; HAR `time` is ms.
    let total_ms: u64 = ev.timing_ticks / 1000;
    let mime_type: &str = if ev.content_type.is_empty() {
        "application/octet-stream"
    } else {
        ev.content_type.as_str()
    };
    serde_json::json!({
        "startedDateTime": ms_to_iso8601(emitted_at_ms),
        "time": total_ms,
        "request": {
            "method": ev.method.as_str(),
            "url": ev.url.as_str(),
            "httpVersion": "HTTP/1.1",
            "cookies": [],
            "headers": [],
            "queryString": [],
            "headersSize": -1,
            "bodySize": -1,
        },
        "response": {
            "status": ev.status_code,
            "statusText": status_text_for(ev.status_code),
            "httpVersion": "HTTP/1.1",
            "cookies": [],
            "headers": [],
            "content": {
                "size": ev.response_body_size_bytes,
                "mimeType": mime_type,
            },
            "redirectURL": "",
            "headersSize": -1,
            "bodySize": -1,
        },
        "cache": {},
        "timings": {
            "send": 0,
            "wait": total_ms,
            "receive": 0,
        },
    })
}

/// Map common HTTP status codes to their reason phrase. Returns `""`
/// for codes outside the small set of values the shim is likely to see;
/// HAR 1.2 permits an empty `statusText` (the field must be a string).
fn status_text_for(status: u32) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "",
    }
}

/// Convert milliseconds since Unix epoch to ISO 8601 string without chrono.
/// Pure integer arithmetic using the proleptic Gregorian calendar.
fn ms_to_iso8601(ms: u64) -> String {
    let secs = ms / 1000;
    let millis = ms % 1000;
    let days = (secs / 86400) as i64;
    let time_secs = secs % 86400;
    let hour = time_secs / 3600;
    let minute = (time_secs % 3600) / 60;
    let second = time_secs % 60;

    // Julian Day Number for 1970-01-01 is 2440588.
    let jdn = 2_440_588_i64 + days;
    let a = jdn + 32_044;
    let b = (4 * a + 3) / 146_097;
    let c = a - (b * 146_097) / 4;
    let d = (4 * c + 3) / 1461;
    let e = c - (1461 * d) / 4;
    let m = (5 * e + 2) / 153;
    let day = e - (153 * m + 2) / 5 + 1;
    let month = m + 3 - 12 * (m / 10);
    let year = 100 * b + d - 4800 + m / 10;

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

fn collect_hex64_strings(val: &serde_json::Value, out: &mut BTreeSet<String>) {
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
