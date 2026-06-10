// LocalManifestWriter implementation — append-only WAL with JCS hash chain.
// prev_hash = sha256(serde_jcs::to_string(prev_entry)).
// HARD #3: serde_json::to_string is BANNED; only serde_jcs::to_string used here.

use crate::error::{LoomError, LoomErrorCode};
use crate::manifest_writer::manifest_writer::{
    AuditKind, LocalManifestWriter, ManifestEntry, ManifestWriter, SessionId, WriterHandle,
};
use ring::digest::{digest, SHA256};
use std::fs;
use std::io::Write;
use std::path::Path;

// ---- helpers ----------------------------------------------------------------

fn sha256_hex(input: &[u8]) -> String {
    let d = digest(&SHA256, input);
    d.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Project a stored manifest line into its HASHABLE form by zeroing the
/// top-level ephemeral fields that vary across two independent same-seed runs:
/// the Header's random `session_id` (a per-session ULID) + wall-clock
/// `started_at_ms`, and every entry's wall-clock `emitted_at_ms`. The fields
/// STAY in the stored line — portable session identity + forensic timestamps are
/// preserved — they are only excluded from the chain hash so the manifest hash
/// chain is byte-equal across two independent same-seed record runs (the
/// determinism property agentic-test-studio relies on).
///
/// Inner `receipt_canonical_bytes` timestamps (started/finished/timing/emitted)
/// are made deterministic at record time via the per-session virtual clock, since
/// they live in an opaque blob that cannot be projected here.
///
/// On any parse/encode failure the raw line is hashed unchanged (never silently
/// drop a line from the chain). Used symmetrically by `append` (computing the
/// next `prev_hash`) and `validate` (re-deriving it), so the chain stays
/// internally consistent.
fn hashable_line(line: &str) -> Vec<u8> {
    // Byte-level field zeroing (NO JSON parse): the chain line is canonical JCS
    // (sorted keys, no whitespace), and these three keys are unambiguous tokens
    // that appear only as object keys (their values are a ULID / hex / number
    // arrays — never the literal `"<key>":` substring). Parsing+re-serializing the
    // whole line (incl. the receipt_canonical_bytes number array, on every append
    // AND every validate line) measurably regressed replay throughput; a targeted
    // scan that zeroes the values in place is effectively free and equally
    // deterministic/consistent between `append` and `validate`. On any unexpected
    // shape the bytes are left unchanged.
    let mut buf = line.as_bytes().to_vec();
    zero_json_string_value(&mut buf, b"\"session_id\":");
    zero_json_number_value(&mut buf, b"\"started_at_ms\":");
    zero_json_number_value(&mut buf, b"\"emitted_at_ms\":");
    buf
}

/// Find the first occurrence of `needle` in `hay` (small needles; naive is fine).
fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Replace the unsigned-integer value following `key` (e.g. `"emitted_at_ms":`)
/// with `0`. No-op if the key is absent or not followed by digits.
fn zero_json_number_value(buf: &mut Vec<u8>, key: &[u8]) {
    let Some(pos) = find_subslice(buf, key) else {
        return;
    };
    let start = pos + key.len();
    let mut end = start;
    while end < buf.len() && buf[end].is_ascii_digit() {
        end += 1;
    }
    if end > start {
        buf.splice(start..end, std::iter::once(b'0'));
    }
}

/// Empty the JSON string value following `key` (e.g. `"session_id":`). No-op if
/// the key is absent or not followed by a quoted string. ULIDs contain no escape
/// sequences, so scanning to the next `"` is sufficient.
fn zero_json_string_value(buf: &mut Vec<u8>, key: &[u8]) {
    let Some(pos) = find_subslice(buf, key) else {
        return;
    };
    let open = pos + key.len();
    if buf.get(open) != Some(&b'"') {
        return;
    }
    let mut close = open + 1;
    while close < buf.len() && buf[close] != b'"' {
        close += 1;
    }
    if close < buf.len() {
        buf.splice(open + 1..close, std::iter::empty());
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// True when this audit kind is part of the v0.9.4 credential-lifecycle
/// surface — the kinds whose canonical_bytes carry a `label` string field
/// subject to the A-W8.5 defense-in-depth validation.
fn is_secret_audit_kind(kind: &AuditKind) -> bool {
    matches!(
        kind,
        AuditKind::SecretOpPending
            | AuditKind::SecretStored
            | AuditKind::SecretFetched
            | AuditKind::SecretDeleted
            | AuditKind::SecretReplaced
            | AuditKind::SecretStoreFailed
            | AuditKind::SecretDeleteFailed
            | AuditKind::SecretFetchFailed
            | AuditKind::PromptBlocked
    )
    // `SecretsListed` and `SecretServiceOwnerChanged` payloads do not carry
    // a `label` field — exclude.
}

/// Canonical label policy (D37): non-empty, ≤64 chars, `[A-Za-z0-9:_-]`.
fn is_canonical_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 64
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == ':' || c == '_' || c == '-')
}

/// Parse `canonical_bytes` as JSON and return the `label` string field
/// when present. Returns `None` for non-JSON / missing-field / non-string
/// payloads — those cases are not the A-W8.5 target (the append itself
/// will surface any JCS-shape problem the same as today).
fn extract_label_field(canonical_bytes: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(canonical_bytes).ok()?;
    match v.get("label")? {
        serde_json::Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// Override the `prev_hash` field in an entry without touching other fields.
/// `ManifestEntry::Header` carries `Option<String>` for prev_hash and is
/// never modified here (the Header is the root of the chain).
fn set_prev_hash(entry: ManifestEntry, hash: String) -> ManifestEntry {
    match entry {
        ManifestEntry::ActionReceipt {
            action_id,
            emitted_at_ms,
            receipt_canonical_bytes,
            ..
        } => ManifestEntry::ActionReceipt {
            action_id,
            emitted_at_ms,
            receipt_canonical_bytes,
            prev_hash: hash,
        },
        ManifestEntry::AuditEntry {
            action_id_ref,
            emitted_at_ms,
            audit_kind,
            canonical_bytes,
            ..
        } => ManifestEntry::AuditEntry {
            action_id_ref,
            emitted_at_ms,
            audit_kind,
            canonical_bytes,
            prev_hash: hash,
        },
        ManifestEntry::SessionTerminal {
            action_id,
            emitted_at_ms,
            reason,
            ..
        } => ManifestEntry::SessionTerminal {
            action_id,
            emitted_at_ms,
            reason,
            prev_hash: hash,
        },
        ManifestEntry::RuntimeCrash {
            last_completed_action_id,
            emitted_at_ms,
            ..
        } => ManifestEntry::RuntimeCrash {
            last_completed_action_id,
            emitted_at_ms,
            prev_hash: hash,
        },
        header @ ManifestEntry::Header { .. } => header,
    }
}

/// Read the last non-empty line from a WAL file.
fn last_wal_line(wal_path: &Path) -> Result<Option<String>, LoomError> {
    let content = fs::read_to_string(wal_path)?;
    Ok(content.lines().last().map(|s| s.to_owned()))
}

/// Build the public entries list from the WAL — only `ActionReceipt` variants.
/// Each entry has: `action_id` (u64), `action` (null), `receipt` (hex-encoded
/// receipt_canonical_bytes), `content_refs` ([]).
/// Used by `export_manifest_json` to produce the public checkpoint.
fn manifest_action_entries_as_json(wal_path: &Path) -> Result<Vec<serde_json::Value>, LoomError> {
    let content = match fs::read_to_string(wal_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(LoomError::from(e)),
    };

    let mut entries = Vec::new();
    for line in content.lines() {
        if line.is_empty() {
            continue;
        }
        let entry: ManifestEntry = serde_json::from_str(line)
            .map_err(|e| LoomError::new(LoomErrorCode::ManifestCorrupt, e.to_string()))?;
        if let ManifestEntry::ActionReceipt {
            action_id,
            receipt_canonical_bytes,
            ..
        } = entry
        {
            let receipt_hex: String = receipt_canonical_bytes
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            entries.push(serde_json::json!({
                "action_id": action_id,
                "action": null,
                "receipt": receipt_hex,
                "content_refs": []
            }));
        }
    }
    Ok(entries)
}

// ---- Public methods on LocalManifestWriter (not part of ManifestWriter trait) ---

impl LocalManifestWriter {
    /// Produce a public-facing `manifest.json` checkpoint in the session directory.
    /// Writes `{"entries": [...]}` with one entry per `ActionReceipt` in the WAL.
    /// Atomic: write to `.json.tmp` → `sync_all` → `rename`.
    pub fn export_manifest_json(&self, session: SessionId) -> Result<(), LoomError> {
        let session_dir = self.sessions_root.join(&session.0);
        let wal_path = session_dir.join("manifest.wal");
        let json_path = session_dir.join("manifest.json");
        let tmp_path = session_dir.join("manifest.json.tmp");

        let entries = manifest_action_entries_as_json(&wal_path)?;
        let doc = serde_json::json!({ "entries": entries });
        let json_bytes = serde_json::to_string_pretty(&doc)
            .map_err(|e| LoomError::internal(format!("manifest.json serialize: {e}")))?;

        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)?;
        f.write_all(json_bytes.as_bytes())?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp_path, &json_path)?;

        Ok(())
    }
}

// ---- ManifestWriter trait impl ----------------------------------------------
// NOTE: open_manifest() is part of the trait so Arc<dyn ManifestWriter> carries it.

impl ManifestWriter for LocalManifestWriter {
    /// Open or create `sessions/<ulid>/manifest.wal`.
    /// New sessions: `O_CREAT|O_EXCL`, writes Header entry (with budgets), fsyncs.
    /// Resumed sessions: opens in append mode without writing a second Header.
    fn open_manifest_with_started_at(
        &self,
        session: SessionId,
        budgets: Option<crate::budget_enforcer::BudgetLimits>,
        started_at_ms_override: Option<u64>,
        capture_policy: Option<String>,
        seed: Option<u64>,
        determinism_enabled: bool,
    ) -> Result<WriterHandle, LoomError> {
        let session_dir = self.sessions_root.join(&session.0);
        fs::create_dir_all(&session_dir)?;

        let wal_path = session_dir.join("manifest.wal");
        let checkpoint_path = session_dir.join("manifest.jsonl");

        let file_result = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&wal_path);

        match file_result {
            Ok(mut file) => {
                // New session — write Header and fsync.
                let header = ManifestEntry::Header {
                    session_id: session.0.clone(),
                    started_at_ms: started_at_ms_override.unwrap_or_else(now_ms),
                    prev_hash: None,
                    budgets,
                    capture_policy,
                    seed,
                    // settle-capture (4b): record ON-ness only when OFF would
                    // be surprising — `Some(false)` is the replay-refuse marker;
                    // `Some(true)` documents the default explicitly. Both
                    // round-trip; legacy headers (None) are treated as ON.
                    determinism_enabled: Some(determinism_enabled),
                };
                let json_line = serde_jcs::to_string(&header)
                    .map_err(|e| LoomError::internal(format!("JCS header: {e}")))?;
                writeln!(file, "{json_line}")?;
                file.sync_all()?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Resumed session — append mode, no second Header.
            }
            Err(e) => return Err(LoomError::from(e)),
        }

        Ok(WriterHandle {
            session_id: session,
            wal_path,
            checkpoint_path,
        })
    }

    fn append(&self, session: SessionId, entry: ManifestEntry) -> Result<(), LoomError> {
        let wal_path = self.sessions_root.join(&session.0).join("manifest.wal");

        // Capture the terminal flag before entry is consumed by set_prev_hash.
        let is_terminal = matches!(entry, ManifestEntry::SessionTerminal { .. });

        // Compute prev_hash from the last WAL line — cached per session, falling
        // back to a full WAL read on a cold cache (first append after open, since
        // the Header is written outside append(); resumed sessions; a fresh writer
        // instance). The cache is a fast path only; a miss is always correct.
        let prev_line: Option<String> = match self.last_line_cache.get(&session) {
            Some(cached) => Some(cached.clone()),
            None => last_wal_line(&wal_path)?,
        };
        let prev_hash = match prev_line {
            Some(last_line) => sha256_hex(&hashable_line(&last_line)),
            None => "0".repeat(64),
        };

        let entry = set_prev_hash(entry, prev_hash);

        // Serialize with JCS (sorted keys) — serde_json::to_string is BANNED here.
        let json_line = serde_jcs::to_string(&entry)
            .map_err(|e| LoomError::internal(format!("JCS entry: {e}")))?;

        let mut file = fs::OpenOptions::new().append(true).open(&wal_path)?;
        writeln!(file, "{json_line}")?;
        file.sync_all()?;

        if is_terminal {
            // Session closing: drop the cache entry, then write the public checkpoint.
            self.last_line_cache.remove(&session);
            self.export_manifest_json(session)?;
        } else {
            // Warm the cache with exactly the line we just wrote. `writeln!` adds
            // the trailing newline, which `last_wal_line`'s `lines().last()` would
            // strip — so the newline-free `json_line` matches the fallback's bytes.
            self.last_line_cache.insert(session, json_line);
        }

        Ok(())
    }

    fn append_audit(
        &self,
        session: SessionId,
        kind: AuditKind,
        canonical_bytes: Vec<u8>,
    ) -> Result<(), LoomError> {
        // A-W8.5: defense-in-depth label validation at the manifest-writer
        // boundary. CLI catches first (clean error message); manifest writer
        // catches as a safety net so a future code path that bypasses CLI
        // validation cannot silently slip a malformed label into the
        // hash-chained audit. Canonical regex: ^[A-Za-z0-9:_-]{1,64}$.
        if is_secret_audit_kind(&kind) {
            if let Some(label) = extract_label_field(&canonical_bytes) {
                if !is_canonical_label(&label) {
                    return Err(LoomError::new(
                        loom_shared::error_format::LoomErrorCode::VaultInvalidLabel,
                        format!(
                            "secret-audit payload label {label:?} fails canonical \
                             validation ^[A-Za-z0-9:_-]{{1,64}}$"
                        ),
                    ));
                }
            }
        }

        self.append(
            session,
            ManifestEntry::AuditEntry {
                action_id_ref: None,
                emitted_at_ms: now_ms(),
                audit_kind: kind,
                canonical_bytes,
                prev_hash: String::new(), // overwritten by append()
            },
        )
    }

    fn checkpoint(&self, session: SessionId) -> Result<(), LoomError> {
        let session_dir = self.sessions_root.join(&session.0);
        let wal_path = session_dir.join("manifest.wal");
        let jsonl_path = session_dir.join("manifest.jsonl");
        let tmp_path = session_dir.join("manifest.jsonl.tmp");

        let content = match fs::read_to_string(&wal_path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(LoomError::from(e)),
        };

        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp_path, &jsonl_path)?;

        Ok(())
    }

    fn validate(&self, session: SessionId) -> Result<(), LoomError> {
        let wal_path = self.sessions_root.join(&session.0).join("manifest.wal");
        let content = fs::read_to_string(&wal_path)?;
        let lines: Vec<&str> = content.lines().collect();

        if lines.len() <= 1 {
            return Ok(()); // Only a Header or empty — nothing to chain-validate.
        }

        for i in 1..lines.len() {
            // Accept EITHER the projected hash (new chains: ephemeral fields
            // excluded → cross-run replay-equal) OR the raw-line hash (manifests
            // recorded before this change, and the old lines of a session resumed
            // across the upgrade). Both bind the non-ephemeral content, so a real
            // tamper changes both and is still caught; this only avoids rejecting
            // pre-existing/legacy chains as corrupt.
            let expected_projected = sha256_hex(&hashable_line(lines[i - 1]));
            let entry: serde_json::Value = serde_json::from_str(lines[i])
                .map_err(|e| LoomError::new(LoomErrorCode::ManifestCorrupt, e.to_string()))?;
            let actual = entry["prev_hash"].as_str().unwrap_or("");
            if actual != expected_projected {
                let expected_raw = sha256_hex(lines[i - 1].as_bytes());
                if actual != expected_raw {
                    return Err(LoomError::new(
                        LoomErrorCode::ManifestCorrupt,
                        format!("hash chain broken at index {i}"),
                    )
                    .with_context(serde_json::json!({
                        "failed_at_index": i,
                        "expected_hash": expected_projected,
                        "expected_hash_legacy": expected_raw,
                        "observed_hash": actual
                    })));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod hashable_line_tests {
    use super::{hashable_line, sha256_hex};

    // Two header lines differing ONLY in the ephemeral session_id + started_at_ms
    // must project to the same bytes (→ same chain hash).
    #[test]
    fn header_projection_ignores_session_id_and_started_at() {
        let a = r#"{"determinism_enabled":true,"kind":"header","seed":42,"session_id":"01run-aaaa","started_at_ms":1000000}"#;
        let b = r#"{"determinism_enabled":true,"kind":"header","seed":42,"session_id":"01run-bbbb","started_at_ms":2222222}"#;
        assert_eq!(hashable_line(a), hashable_line(b));
        // ...but a different SEED (content) must differ.
        let c = r#"{"determinism_enabled":true,"kind":"header","seed":7,"session_id":"01run-aaaa","started_at_ms":1000000}"#;
        assert_ne!(hashable_line(a), hashable_line(c));
    }

    // Receipt lines differing only in emitted_at_ms project equal; differing in
    // receipt_canonical_bytes (content) differ. Guards against the projection
    // mis-zeroing or matching the wrong occurrence inside the byte array.
    #[test]
    fn receipt_projection_ignores_emitted_at_ms_but_not_content() {
        let a = r#"{"action_id":1,"emitted_at_ms":111,"kind":"action_receipt","prev_hash":"x","receipt_canonical_bytes":[1,2,3]}"#;
        let b = r#"{"action_id":1,"emitted_at_ms":999999,"kind":"action_receipt","prev_hash":"x","receipt_canonical_bytes":[1,2,3]}"#;
        assert_eq!(hashable_line(a), hashable_line(b));
        let c = r#"{"action_id":1,"emitted_at_ms":111,"kind":"action_receipt","prev_hash":"x","receipt_canonical_bytes":[1,2,4]}"#;
        assert_ne!(hashable_line(a), hashable_line(c));
    }

    // The byte-array values are preserved verbatim (only the emitted_at_ms scalar
    // is zeroed). A number array can never contain the literal `"emitted_at_ms":`
    // token, so the first-occurrence scan only hits the real key.
    #[test]
    fn projection_preserves_byte_array_and_zeros_only_the_scalar() {
        let line = r#"{"action_id":2,"emitted_at_ms":5,"kind":"action_receipt","prev_hash":"p","receipt_canonical_bytes":[34,101,109,105]}"#;
        let projected = String::from_utf8(hashable_line(line)).unwrap();
        assert!(
            projected.contains(r#""emitted_at_ms":0"#),
            "emitted_at_ms zeroed"
        );
        assert!(
            projected.contains("[34,101,109,105]"),
            "byte array preserved verbatim"
        );
    }

    // Determinism: projecting the same line twice yields identical bytes.
    #[test]
    fn projection_is_deterministic() {
        let line = r#"{"action_id":1,"emitted_at_ms":111,"kind":"action_receipt","prev_hash":"x","receipt_canonical_bytes":[9]}"#;
        assert_eq!(
            sha256_hex(&hashable_line(line)),
            sha256_hex(&hashable_line(line))
        );
    }
}
