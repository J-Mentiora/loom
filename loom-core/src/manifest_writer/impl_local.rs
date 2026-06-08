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

        // Compute prev_hash from the last line already in the WAL.
        let prev_hash = match last_wal_line(&wal_path)? {
            Some(last_line) => sha256_hex(last_line.as_bytes()),
            None => "0".repeat(64),
        };

        let entry = set_prev_hash(entry, prev_hash);

        // Serialize with JCS (sorted keys) — serde_json::to_string is BANNED here.
        let json_line = serde_jcs::to_string(&entry)
            .map_err(|e| LoomError::internal(format!("JCS entry: {e}")))?;

        let mut file = fs::OpenOptions::new().append(true).open(&wal_path)?;
        writeln!(file, "{json_line}")?;
        file.sync_all()?;

        // Produce manifest.json checkpoint when session closes.
        if is_terminal {
            self.export_manifest_json(session)?;
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
            let expected = sha256_hex(lines[i - 1].as_bytes());
            let entry: serde_json::Value = serde_json::from_str(lines[i])
                .map_err(|e| LoomError::new(LoomErrorCode::ManifestCorrupt, e.to_string()))?;
            let actual = entry["prev_hash"].as_str().unwrap_or("");
            if actual != expected {
                return Err(LoomError::new(
                    LoomErrorCode::ManifestCorrupt,
                    format!("hash chain broken at index {i}"),
                )
                .with_context(serde_json::json!({
                    "failed_at_index": i,
                    "expected_hash": expected,
                    "observed_hash": actual
                })));
            }
        }
        Ok(())
    }
}
