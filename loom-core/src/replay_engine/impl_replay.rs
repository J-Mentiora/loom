// LocalReplayEngine implementation — bit-equal structural replay, diff, inspect, validate.
//
// AC coverage:
//   AC-REPLAY-01.1: replay() copies receipt_canonical_bytes byte-for-byte
//   AC-REPLAY-01.2: pre-flight checks non-screenshot content_refs in CAS
//   AC-REPLAY-01.3: install_replay_mode(tape) called before replay writes
//   AC-REPLAY-02.1: diff() computes action_count_delta
//   AC-REPLAY-02.2: diff() compares receipt fields by key
//   AC-REPLAY-02.3: screenshot fields routed to screenshot_diffs, not field_diffs
//   AC-REPLAY-03.1: inspect() reads WAL 0..=at_action without mutation
//   AC-PERF-03.1:   structural replay is disk I/O only (no WASM)

use crate::content_store::ContentRef;
use crate::determinism_harness::SideEffectTape;
use crate::error::{LoomError, LoomErrorCode};
use crate::manifest_writer::{AuditKind, ManifestEntry, SessionId};
use crate::replay_engine::replay_engine::{
    DiffOpts, DiffReport, FieldDiff, LocalReplayEngine, ReplayEngine, ReplayOpts, ValidationResult,
};
use crate::session_manager::SessionCreateOpts;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Read all manifest entries from a WAL file.
fn read_wal_entries(wal_path: &std::path::Path) -> Result<Vec<ManifestEntry>, LoomError> {
    let content = match std::fs::read_to_string(wal_path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(LoomError::from(e)),
        Ok(c) => c,
    };
    let mut entries = Vec::new();
    for line in content.lines() {
        if line.is_empty() {
            continue;
        }
        let entry: ManifestEntry = serde_json::from_str(line)
            .map_err(|e| LoomError::new(LoomErrorCode::ManifestCorrupt, e.to_string()))?;
        entries.push(entry);
    }
    Ok(entries)
}

/// Check if a receipt field name refers to a screenshot artifact.
fn is_screenshot_field(field: &str) -> bool {
    field.contains("screenshot") || field == "screen_hash"
}

/// Collect (sha256, kind) pairs from `content_refs` array in a receipt.
fn collect_content_refs(receipt_bytes: &[u8]) -> Vec<(String, String)> {
    let Ok(val) = serde_json::from_slice::<serde_json::Value>(receipt_bytes) else {
        return vec![];
    };
    let Some(refs) = val.get("content_refs").and_then(|r| r.as_array()) else {
        return vec![];
    };
    refs.iter()
        .filter_map(|r| {
            let sha256 = r.get("sha256")?.as_str()?.to_string();
            let kind = r
                .get("kind")
                .and_then(|k| k.as_str())
                .unwrap_or("")
                .to_string();
            Some((sha256, kind))
        })
        .collect()
}

/// Compare two JSON objects field by field, collecting diffs.
fn compare_receipt_fields(
    action_id: u64,
    a: &serde_json::Value,
    b: &serde_json::Value,
    field_diffs: &mut Vec<FieldDiff>,
    screenshot_diffs: &mut Vec<u64>,
    _opts: &DiffOpts,
) {
    let a_obj = match a.as_object() {
        Some(o) => o,
        None => return,
    };
    let b_obj = match b.as_object() {
        Some(o) => o,
        None => return,
    };

    // Check all fields in A against B
    for (key, a_val) in a_obj {
        let b_val = b_obj.get(key).unwrap_or(&serde_json::Value::Null);
        if a_val != b_val {
            if is_screenshot_field(key) {
                screenshot_diffs.push(action_id);
            } else {
                field_diffs.push(FieldDiff {
                    action_id,
                    field_path: format!("entries[{action_id}].receipt.{key}"),
                    source_value: a_val.to_string(),
                    replay_value: b_val.to_string(),
                });
            }
        }
    }
    // Check fields in B not in A
    for (key, b_val) in b_obj {
        if !a_obj.contains_key(key) {
            if is_screenshot_field(key) {
                screenshot_diffs.push(action_id);
            } else {
                field_diffs.push(FieldDiff {
                    action_id,
                    field_path: format!("entries[{action_id}].receipt.{key}"),
                    source_value: serde_json::Value::Null.to_string(),
                    replay_value: b_val.to_string(),
                });
            }
        }
    }
}

// ---- Direct methods on LocalReplayEngine (helpers, not part of trait) ----

impl LocalReplayEngine {
    /// Inspect a session's WAL up to (and including) `at_action`. Read-only.
    pub fn inspect(
        &self,
        session_id: SessionId,
        at_action: Option<u64>,
    ) -> Result<serde_json::Value, LoomError> {
        let wal_path = self.sessions_root.join(&session_id.0).join("manifest.wal");
        let entries = read_wal_entries(&wal_path)?;

        let filtered: Vec<serde_json::Value> = entries
            .iter()
            .filter_map(|e| {
                if let ManifestEntry::ActionReceipt {
                    action_id,
                    emitted_at_ms,
                    receipt_canonical_bytes,
                    ..
                } = e
                {
                    if at_action.is_none_or(|max| *action_id <= max) {
                        let receipt: serde_json::Value =
                            serde_json::from_slice(receipt_canonical_bytes)
                                .unwrap_or(serde_json::Value::Null);
                        return Some(serde_json::json!({
                            "action_id": action_id,
                            "emitted_at_ms": emitted_at_ms,
                            "receipt": receipt,
                        }));
                    }
                }
                None
            })
            .collect();

        Ok(serde_json::json!({
            "session_id": session_id.0,
            "at_action": at_action,
            "action_count": filtered.len(),
            "entries": filtered,
        }))
    }

    /// Validate hash chain integrity + blob presence for a session.
    pub fn validate(&self, session_id: SessionId) -> Result<ValidationResult, LoomError> {
        let mut reasons = Vec::new();

        // 1. Hash chain check
        if let Err(e) = self.manifest_writer.validate(session_id.clone()) {
            reasons.push(format!("chain: {}", e.message));
        }

        // 2. Blob presence check
        let wal_path = self.sessions_root.join(&session_id.0).join("manifest.wal");
        if let Ok(entries) = read_wal_entries(&wal_path) {
            for entry in &entries {
                if let ManifestEntry::ActionReceipt {
                    receipt_canonical_bytes,
                    ..
                } = entry
                {
                    for (sha256, kind) in collect_content_refs(receipt_canonical_bytes) {
                        if kind != "screenshot" {
                            let cr = ContentRef {
                                sha256: sha256.clone(),
                                size_bytes: 0,
                            };
                            if self.content_store.get(&cr).is_err() {
                                reasons.push(format!(
                                    "StoreNotFound: missing blob {sha256} (kind: {kind})"
                                ));
                            }
                        }
                    }
                }
            }
        }

        Ok(ValidationResult {
            session_id: session_id.0,
            passed: reasons.is_empty(),
            reasons,
        })
    }
}

// ---- ReplayEngine trait impl ----

impl ReplayEngine for LocalReplayEngine {
    fn replay(&self, source: SessionId, opts: ReplayOpts) -> Result<SessionId, LoomError> {
        let _ = &opts; // structural replay does not vary by opts
                       // 1. Validate source hash chain
        self.manifest_writer.validate(source.clone())?;

        // 1b. Refuse to replay sessions that didn't end cleanly. The
        // action chain in an aborted/crashed session may be incomplete
        // (operator abandoned mid-flow, runtime crashed before the
        // session-terminal flush) and replaying it produces a session
        // that LOOKS green but represents an abandoned trace. Default-
        // deny here surfaces a typed `SessionAborted` error; operators
        // who genuinely want to replay an abandoned trace can reopen
        // the source session's WAL and re-issue the actions explicitly.
        // Phase 8 round-23 finding.
        let source_wal = self.sessions_root.join(&source.0).join("manifest.wal");
        if let Ok(content) = std::fs::read_to_string(&source_wal) {
            let mut terminal_kind: Option<String> = None;
            let mut crashed = false;
            for line in content.lines() {
                if line.is_empty() {
                    continue;
                }
                if let Ok(entry) = serde_json::from_str::<ManifestEntry>(line) {
                    match entry {
                        ManifestEntry::SessionTerminal { reason, .. } => {
                            terminal_kind = Some(reason);
                        }
                        ManifestEntry::RuntimeCrash { .. } => {
                            crashed = true;
                        }
                        _ => {}
                    }
                }
            }
            if crashed {
                return Err(LoomError::new(
                    LoomErrorCode::SessionAborted,
                    format!(
                        "session {} crashed mid-flow; replay refuses to reproduce a partial trace",
                        source.0
                    ),
                ));
            }
            if let Some(reason) = terminal_kind {
                if reason != "close" && reason != "replay_complete" {
                    return Err(LoomError::new(
                        LoomErrorCode::SessionAborted,
                        format!(
                            "session {} ended via abort (reason={reason}); replay refuses to reproduce an abandoned trace",
                            source.0
                        ),
                    ));
                }
            }
        }

        // 2. Load side-effect tape
        let tape = SideEffectTape::load_from_file(&self.sessions_root, &source.0)?;

        // 3. Pre-flight: check non-screenshot content_refs are present in CAS
        let wal_path = self.sessions_root.join(&source.0).join("manifest.wal");
        let entries = read_wal_entries(&wal_path)?;
        for entry in &entries {
            if let ManifestEntry::ActionReceipt {
                receipt_canonical_bytes,
                ..
            } = entry
            {
                for (sha256, kind) in collect_content_refs(receipt_canonical_bytes) {
                    if kind != "screenshot" {
                        let cr = ContentRef {
                            sha256: sha256.clone(),
                            size_bytes: 0,
                        };
                        if self.content_store.get(&cr).is_err() {
                            return Err(LoomError::new(
                                LoomErrorCode::ReplayMissingBlob,
                                format!("pre-flight: missing blob {sha256} (kind: {kind})"),
                            ));
                        }
                    }
                }
            }
        }

        // 4. Install tape-driven determinism
        let _table = self.determinism.install_replay_mode(tape);

        // 4b. Extract the source session's Header started_at_ms so the
        // replay Header carries the same timestamp. The hash chain
        // chains over the Header's canonical bytes, so any divergence
        // here poisons every subsequent prev_hash. AC-SHCRT-08.
        let source_started_at_ms = entries.iter().find_map(|e| {
            if let ManifestEntry::Header { started_at_ms, .. } = e {
                Some(*started_at_ms)
            } else {
                None
            }
        });

        // 5. Create replay session via SessionManager
        let replay_id = self.session_manager.create(SessionCreateOpts {
            agent_id: "replay-engine".to_string(),
            surface: "replay".to_string(),
            seed: None,
            limits: None,
            replay_of: Some(source.clone()),
            started_at_ms_override: source_started_at_ms,
            capture_policy: None,
            no_blocklist: false,
            // Replay sessions inherit the source's profile via the manifest;
            // the gate is a daemon-layer concern that doesn't fire on replay
            // (no live shim). Default-safe is fine for the in-memory copy.
            profile: "safe".to_string(),
        })?;

        // 6. Copy ActionReceipt + BlockedUrl AuditEntry entries with original
        //    emitted_at_ms (AC-REPLAY-01.1, AC-BLOCKLIST-03). Source order is
        //    preserved so the replay manifest's prev_hash chain is bit-equal
        //    to the source's at every line index.
        //
        //    EXPLICIT ALLOWLIST — DO NOT replace with a fall-through that
        //    matches all `AuditEntry` variants. Vault grant audits, FSM
        //    transitions, etc. are intentionally NOT re-emitted; their
        //    replay-chain behavior is unchanged from before AC-BLOCKLIST-03.
        for entry in &entries {
            match entry {
                ManifestEntry::ActionReceipt {
                    action_id,
                    emitted_at_ms,
                    receipt_canonical_bytes,
                    ..
                } => {
                    self.manifest_writer.append(
                        replay_id.clone(),
                        ManifestEntry::ActionReceipt {
                            action_id: *action_id,
                            emitted_at_ms: *emitted_at_ms, // copy from source — preserves bit-equality
                            receipt_canonical_bytes: receipt_canonical_bytes.clone(),
                            prev_hash: String::new(), // overwritten by LocalManifestWriter::append()
                        },
                    )?;
                }
                ManifestEntry::AuditEntry {
                    action_id_ref,
                    emitted_at_ms,
                    audit_kind: AuditKind::BlockedUrl,
                    canonical_bytes,
                    ..
                } => {
                    self.manifest_writer.append(
                        replay_id.clone(),
                        ManifestEntry::AuditEntry {
                            action_id_ref: *action_id_ref,
                            emitted_at_ms: *emitted_at_ms,
                            audit_kind: AuditKind::BlockedUrl,
                            canonical_bytes: canonical_bytes.clone(),
                            prev_hash: String::new(), // overwritten by append()
                        },
                    )?;
                }
                _ => {}
            }
        }

        // 7. Write SessionTerminal to close the replay session
        self.manifest_writer.append(
            replay_id.clone(),
            ManifestEntry::SessionTerminal {
                action_id: 0,
                emitted_at_ms: now_ms(),
                reason: "replay_complete".to_string(),
                prev_hash: String::new(),
            },
        )?;

        Ok(replay_id)
    }

    fn diff(&self, a: SessionId, b: SessionId, opts: DiffOpts) -> Result<DiffReport, LoomError> {
        let wal_a = self.sessions_root.join(&a.0).join("manifest.wal");
        let wal_b = self.sessions_root.join(&b.0).join("manifest.wal");

        // Extract ActionReceipt entries from each WAL
        let entries_a: Vec<(u64, Vec<u8>)> = read_wal_entries(&wal_a)?
            .into_iter()
            .filter_map(|e| {
                if let ManifestEntry::ActionReceipt {
                    action_id,
                    receipt_canonical_bytes,
                    ..
                } = e
                {
                    Some((action_id, receipt_canonical_bytes))
                } else {
                    None
                }
            })
            .collect();

        let entries_b: Vec<(u64, Vec<u8>)> = read_wal_entries(&wal_b)?
            .into_iter()
            .filter_map(|e| {
                if let ManifestEntry::ActionReceipt {
                    action_id,
                    receipt_canonical_bytes,
                    ..
                } = e
                {
                    Some((action_id, receipt_canonical_bytes))
                } else {
                    None
                }
            })
            .collect();

        let action_count_delta = (entries_b.len() as i64) - (entries_a.len() as i64);

        let mut field_diffs = Vec::new();
        let mut screenshot_diffs: Vec<u64> = Vec::new();

        // Compare matching action_ids
        for (a_id, a_bytes) in &entries_a {
            if let Some((_, b_bytes)) = entries_b.iter().find(|(id, _)| id == a_id) {
                let a_val: serde_json::Value = serde_json::from_slice(a_bytes)
                    .unwrap_or(serde_json::Value::Object(Default::default()));
                let b_val: serde_json::Value = serde_json::from_slice(b_bytes)
                    .unwrap_or(serde_json::Value::Object(Default::default()));

                if a_val != b_val {
                    compare_receipt_fields(
                        *a_id,
                        &a_val,
                        &b_val,
                        &mut field_diffs,
                        &mut screenshot_diffs,
                        &opts,
                    );
                }
            }
        }

        // When exclude_screenshots is true, screenshot diffs are still tracked in
        // screenshot_diffs[] but never in field_diffs[] (AC-REPLAY-02.3).
        // When exclude_screenshots is false, screenshot diffs appear only in
        // screenshot_diffs[], never in field_diffs[] (same rule — screenshots are
        // always a separate bucket).

        Ok(DiffReport {
            a,
            b,
            action_count_delta,
            field_diffs,
            screenshot_diffs,
        })
    }
}
