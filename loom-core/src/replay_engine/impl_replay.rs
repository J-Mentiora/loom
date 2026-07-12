// LocalReplayEngine implementation — bit-equal structural replay, diff, inspect, validate.
//
// Behaviour:
//   replay() copies receipt_canonical_bytes byte-for-byte
//   pre-flight checks non-screenshot content_refs in CAS
//   install_replay_mode(tape) called before replay writes
//   diff() computes action_count_delta
//   diff() compares receipt fields by key
//   screenshot fields routed to screenshot_diffs, not field_diffs
//   inspect() reads WAL 0..=at_action without mutation
//   structural replay is disk I/O only (no WASM)

use crate::content_store::ContentRef;
use crate::determinism_harness::SideEffectTape;
use crate::error::{LoomError, LoomErrorCode};
use crate::manifest_writer::{AuditKind, ManifestEntry, SessionId};
use crate::replay_engine::replay_engine::{
    DiffOpts, DiffReport, FieldDiff, LocalReplayEngine, ReplayEngine, ReplayOpts, ValidationResult,
};
use crate::session_manager::SessionCreateOpts;

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

/// Map a receipt field name to the artifact blob-kind it carries, if any.
///
/// Artifact blobs (screenshots, screencast video) hold non-deterministic bytes
/// — encoder/timing variation makes them differ run-to-run — so they are
/// EXCLUDED from the replay hash chain and from field-diff reporting; only their
/// content hash is recorded. This replaces the old `field.contains("screenshot")
/// || field == "screen_hash"` check with an explicit field→kind map so new
/// artifact kinds (e.g. `screencast_*`) are excluded deliberately rather than by
/// a brittle substring, and the `screen_hash` legacy name is data, not a
/// hard-coded special case. Keep this in sync with `EXCLUDED_BLOB_KINDS`.
fn replay_blob_kind(field: &str) -> Option<&'static str> {
    // EXACT field names (no substring matching) — a `contains()` check is exactly
    // the brittleness this map set out to kill (a future `screen_metrics` /
    // `screencast_config` field must NOT be silently excluded from the chain).
    match field {
        // Screenshot artifact byte-carriers: legacy `screen_hash`/`screenshot_hash`
        // + the current `screenshot_after_*` / `screenshot_before_*` family.
        "screen_hash"
        | "screenshot_hash"
        | "screenshot_after_hash"
        | "screenshot_after_blob_ref"
        | "screenshot_before_blob_ref" => Some("screenshot"),
        "screencast_after_hash" | "screencast_after_blob_ref" => Some("screencast"),
        _ => None,
    }
}

/// Blob kinds excluded from the replay hash chain / field diffs (NFR-DET-01).
const EXCLUDED_BLOB_KINDS: &[&str] = &["screenshot", "screencast"];

/// True when a receipt field carries non-deterministic artifact bytes that are
/// excluded from the replay chain (see [`replay_blob_kind`]).
fn is_excluded_artifact_field(field: &str) -> bool {
    replay_blob_kind(field).is_some_and(|k| EXCLUDED_BLOB_KINDS.contains(&k))
}

/// Replay refusal 1b — non-clean source. Returns the typed `SessionAborted`
/// refusal when the source crashed mid-flow or ended via abort; `None` for
/// clean (or unreadable — the chain validate owns that failure) sources.
/// Shared by `replay()` and `validate()` so the `replayable` verdict can
/// never drift from what replay actually refuses.
pub fn unclean_source_refusal(
    sessions_root: &std::path::Path,
    source: &SessionId,
) -> Option<LoomError> {
    let source_wal = sessions_root.join(&source.0).join("manifest.wal");
    let content = std::fs::read_to_string(&source_wal).ok()?;
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
        return Some(LoomError::new(
            LoomErrorCode::SessionAborted,
            format!(
                "session {} crashed mid-flow; replay refuses to reproduce a partial trace",
                source.0
            ),
        ));
    }
    if let Some(reason) = terminal_kind {
        if reason != "close" && reason != "replay_complete" {
            return Some(LoomError::new(
                LoomErrorCode::SessionAborted,
                format!(
                    "session {} ended via abort (reason={reason}); replay refuses to reproduce an abandoned trace",
                    source.0
                ),
            ));
        }
    }
    None
}

/// Replay refusal 4b — non-deterministic source (settle-capture). A
/// `--no-determinism` recording ran with real wall-clock + unseeded RNG, so
/// its receipts can never be reproduced. `None`/`Some(true)` Header flags
/// (legacy + deterministic) replay normally. Typed `NotReplayable` so the
/// wire never degrades this to a request-shape error. Shared by `replay()`
/// and `validate()` (see `unclean_source_refusal`).
pub fn non_deterministic_refusal(
    entries: &[ManifestEntry],
    source: &SessionId,
) -> Option<LoomError> {
    let source_determinism_enabled = entries.iter().find_map(|e| {
        if let ManifestEntry::Header {
            determinism_enabled,
            ..
        } = e
        {
            *determinism_enabled
        } else {
            None
        }
    });
    if source_determinism_enabled == Some(false) {
        return Some(LoomError::new(
            LoomErrorCode::NotReplayable,
            format!(
                "session {} was recorded with --no-determinism (real clock + unseeded RNG) \
                 and is NOT replayable: a non-deterministic run can never be replay-equal",
                source.0
            ),
        ));
    }
    None
}

/// Collect `(sha256, kind)` blob references from a receipt.
///
/// Production receipts (`ReceiptPayload`, see receipt_builder.rs) carry blob
/// references in NAMED `{sha256, size_bytes}` fields, NOT in a `content_refs`
/// array — the only `content_refs` producer in the workspace is
/// `export_manifest_json`'s hardcoded empty array and test fixtures. Reading
/// only `content_refs` (as this did before audit 2026-06-10, F32) made
/// blob-presence validation vacuous for real sessions: `validate()` silently
/// passed and the `ReplayMissingBlob` pre-flight could never fire when a CAS
/// blob referenced by a real recording was missing.
///
/// This now walks the actual `ReceiptPayload` blob-ref fields:
/// `dom_after_blob_ref` / `dom_before_blob_ref` (kind "dom"),
/// `return_value_blob_ref` (kind "return_value"),
/// `network_events[].response_body_ref` (kind "network"), and
/// `screenshot_after_blob_ref` / `screenshot_before_blob_ref` (kind
/// "screenshot").
///
/// The "screenshot" kind is load-bearing: callers skip screenshots when
/// deciding whether a missing blob aborts replay (screenshots are excluded
/// from the integrity gate by design — NFR-DET-01). The legacy `content_refs`
/// array is still honored so existing fixtures keep working.
///
/// Shared by `LocalReplayEngine::validate`, the `replay()` pre-flight, and
/// `CoreApiFacade::validate_session_result` so all three agree on the real
/// receipt shape.
pub fn collect_content_refs(receipt_bytes: &[u8]) -> Vec<(String, String)> {
    let Ok(val) = serde_json::from_slice::<serde_json::Value>(receipt_bytes) else {
        return vec![];
    };
    let mut out = Vec::new();

    // A named `{sha256, size_bytes}` blob-ref field.
    let mut push_named = |field: &str, kind: &str| {
        if let Some(sha256) = val
            .get(field)
            .and_then(|r| r.get("sha256"))
            .and_then(|s| s.as_str())
        {
            out.push((sha256.to_string(), kind.to_string()));
        }
    };
    push_named("dom_after_blob_ref", "dom");
    push_named("dom_before_blob_ref", "dom");
    push_named("return_value_blob_ref", "return_value");
    push_named("screenshot_after_blob_ref", "screenshot");
    push_named("screenshot_before_blob_ref", "screenshot");
    push_named("screencast_after_blob_ref", "screencast");

    // Per-request response-body blobs on navigate-tier receipts.
    if let Some(events) = val.get("network_events").and_then(|e| e.as_array()) {
        for ev in events {
            if let Some(sha256) = ev
                .get("response_body_ref")
                .and_then(|r| r.get("sha256"))
                .and_then(|s| s.as_str())
            {
                out.push((sha256.to_string(), "network".to_string()));
            }
        }
    }

    // Legacy `content_refs` array (export_manifest_json + test fixtures).
    if let Some(refs) = val.get("content_refs").and_then(|r| r.as_array()) {
        for r in refs {
            if let Some(sha256) = r.get("sha256").and_then(|s| s.as_str()) {
                let kind = r
                    .get("kind")
                    .and_then(|k| k.as_str())
                    .unwrap_or("")
                    .to_string();
                out.push((sha256.to_string(), kind));
            }
        }
    }

    out
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
            if is_excluded_artifact_field(key) {
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
            if is_excluded_artifact_field(key) {
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
    ///
    /// `passed` covers integrity only; `replayable` additionally applies
    /// the replay-refusal checks (crashed/aborted source, `--no-determinism`
    /// recording), so PASS ≠ replayable.
    pub fn validate(&self, session_id: SessionId) -> Result<ValidationResult, LoomError> {
        let mut reasons = Vec::new();

        // 1. Hash chain check
        if let Err(e) = self.manifest_writer.validate(session_id.clone()) {
            reasons.push(format!("chain: {}", e.message));
        }

        // 2. Blob presence check
        let mut determinism_refusal = None;
        let wal_path = self.sessions_root.join(&session_id.0).join("manifest.wal");
        if let Ok(entries) = read_wal_entries(&wal_path) {
            for entry in &entries {
                if let ManifestEntry::ActionReceipt {
                    receipt_canonical_bytes,
                    ..
                } = entry
                {
                    for (sha256, kind) in collect_content_refs(receipt_canonical_bytes) {
                        if !EXCLUDED_BLOB_KINDS.contains(&kind.as_str()) {
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
            determinism_refusal = non_deterministic_refusal(&entries, &session_id);
        }

        // 3. Replayability verdict — mirrors `replay()`'s refusal order
        // (unclean source, then determinism) via the shared helpers, then
        // folds in integrity: a failed chain/blob check is refused by
        // replay's own pre-flight too.
        let refusal = unclean_source_refusal(&self.sessions_root, &session_id)
            .or(determinism_refusal)
            .map(|e| e.message);
        let passed = reasons.is_empty();
        let (replayable, not_replayable_reason) = match refusal {
            Some(reason) => (false, Some(reason)),
            None if !passed => (false, Some("validation failed (see reasons)".to_string())),
            None => (true, None),
        };

        Ok(ValidationResult {
            session_id: session_id.0,
            passed,
            reasons,
            replayable,
            not_replayable_reason,
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
        // Late-stage testing finding. (Shared with `validate()`'s
        // `replayable` verdict via `unclean_source_refusal`.)
        if let Some(refusal) = unclean_source_refusal(&self.sessions_root, &source) {
            return Err(refusal);
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
                    if !EXCLUDED_BLOB_KINDS.contains(&kind.as_str()) {
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
        // here poisons every subsequent prev_hash.
        let source_started_at_ms = entries.iter().find_map(|e| {
            if let ManifestEntry::Header { started_at_ms, .. } = e {
                Some(*started_at_ms)
            } else {
                None
            }
        });

        // settle-capture (D3): reconstruct the source session's determinism
        // seed from its recorded Header. Before this fix the replay session was
        // always created with `seed: None` → `Seed(default_seed)`, so a
        // `--seed N` recording replayed with a DIFFERENT in-Chromium
        // Math.random/Date.now and diverged. `None` for legacy headers (no seed
        // field) preserves the prior default-seed behaviour.
        let source_seed = entries.iter().find_map(|e| {
            if let ManifestEntry::Header { seed, .. } = e {
                *seed
            } else {
                None
            }
        });

        // Header fidelity (audit 2026-06-10): reconstruct the source Header's
        // recorded budgets + capture_policy the same way the seed is
        // reconstructed above. Both fields serialize with
        // `skip_serializing_if`, so a source recorded with `--budget` /
        // `--capture-policy` has DIFFERENT canonical Header bytes than a
        // default one; `hashable_line()` only projects out
        // session_id/started_at_ms/emitted_at_ms, so dropping them here made
        // the replay Header's projected hash diverge from the source's —
        // poisoning every subsequent prev_hash in the replay chain. Passing
        // `limits` through is Header-only on the replay path:
        // `LocalSessionManager::create` skips budget enforcement entirely when
        // `replay_of` is set, so no wall-clock timer or kill callback is
        // re-armed for the replay session.
        let (source_budgets, source_capture_policy) = entries
            .iter()
            .find_map(|e| {
                if let ManifestEntry::Header {
                    budgets,
                    capture_policy,
                    ..
                } = e
                {
                    Some((*budgets, capture_policy.clone()))
                } else {
                    None
                }
            })
            .unwrap_or((None, None));

        // settle-capture (4b): REFUSE to replay a non-deterministic session.
        // A `--no-determinism` recording ran with real wall-clock + unseeded
        // RNG, so its receipts can never be reproduced — replaying it would
        // silently echo the recorded bytes and imply a reproducibility the run
        // never had. This is the safety pair of the Header flag (NFR-DET-01).
        // Typed `NotReplayable` (replay-refusal fidelity): this used to be
        // `InvalidArgument`, which the daemon wire degraded to a generic
        // `schema_violation` — pointing the caller at the wrong fix. (Shared
        // with `validate()`'s `replayable` verdict via
        // `non_deterministic_refusal`.)
        if let Some(refusal) = non_deterministic_refusal(&entries, &source) {
            return Err(refusal);
        }

        // 5. Create replay session via SessionManager
        let replay_id = self.session_manager.create(SessionCreateOpts {
            agent_id: "replay-engine".to_string(),
            surface: "replay".to_string(),
            seed: source_seed,
            // Recorded source budgets — Header-only on the replay path (no
            // enforcement is armed for `replay_of` sessions, see above).
            limits: source_budgets,
            replay_of: Some(source.clone()),
            started_at_ms_override: source_started_at_ms,
            capture_policy: source_capture_policy,
            no_blocklist: false,
            // The replay session itself is deterministic by construction (we
            // refused non-deterministic sources above).
            no_determinism: false,
            record_screencast: false,
            audio: false,
            // Replay sessions inherit the source's profile via the manifest;
            // the gate is a daemon-layer concern that doesn't fire on replay
            // (no live shim). Default-safe is fine for the in-memory copy.
            profile: "safe".to_string(),
        })?;

        // 6. Copy ActionReceipt + BlockedUrl AuditEntry entries with original
        //    emitted_at_ms. Source order is
        //    preserved so the replay manifest's prev_hash chain is bit-equal
        //    to the source's at every line index.
        //
        //    EXPLICIT ALLOWLIST — DO NOT replace with a fall-through that
        //    matches all `AuditEntry` variants. Vault grant audits, FSM
        //    transitions, etc. are intentionally NOT re-emitted; their
        //    replay-chain behavior is unchanged from before BlockedUrl was
        //    added to the replay-eligible set.
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

        // 7. Close the replay session THROUGH the session manager so the
        //    in-memory FSM transition (Active → Closed), the scope cancel,
        //    and the SessionTerminal append happen once, coherently.
        //    `close_with_reason` writes the exact same terminal payload this
        //    step used to append directly via `manifest_writer` — bypassing
        //    the manager left the in-memory session Active with
        //    `last_activity_ms` pinned to the SOURCE's original
        //    `started_at_ms` (the epoch of the recording!), so the daemon's
        //    idle reaper saw it as instantly idle and appended a SECOND
        //    SessionTerminal{idle_ttl} over the completed replay manifest,
        //    flipping its on-disk status to `aborted:idle_ttl` and blocking
        //    replay-of-replay.
        self.session_manager
            .close_with_reason(replay_id.clone(), "replay_complete")?;

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
        // screenshot_diffs[] but never in field_diffs[].
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

#[cfg(test)]
mod blob_kind_tests {
    use super::{is_excluded_artifact_field, replay_blob_kind};

    #[test]
    fn screencast_and_screenshot_fields_are_excluded() {
        for f in [
            "screenshot_after_hash",
            "screenshot_after_blob_ref",
            "screenshot_before_blob_ref",
            "screen_hash",
            "screencast_after_hash",
            "screencast_after_blob_ref",
        ] {
            assert!(is_excluded_artifact_field(f), "{f} must be excluded");
        }
        assert_eq!(
            replay_blob_kind("screencast_after_hash"),
            Some("screencast")
        );
        assert_eq!(
            replay_blob_kind("screenshot_after_hash"),
            Some("screenshot")
        );
    }

    #[test]
    fn unrelated_screen_prefixed_fields_are_not_excluded() {
        // The exact-match map (R7) must NOT catch a future field that merely
        // shares the `screen*` prefix — that was the substring brittleness.
        for f in [
            "screen_metrics",
            "screencast_config",
            "screenshot_count",
            "dom_after_blob_ref",
            "url",
        ] {
            assert!(
                !is_excluded_artifact_field(f),
                "{f} must NOT be excluded (no substring matching)"
            );
        }
    }
}
