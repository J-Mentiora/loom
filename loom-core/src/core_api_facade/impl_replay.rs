// CoreApiFacade replay/diff/inspect/validate — bridges CoreFacadeBridge methods
// to loom-core internals.
//
// Behaviour:
//   replay_session_to_id → ReplayEngine::replay
//   diff_sessions_json → ReplayEngine::diff
//   inspect_session_json → WAL read (read-only)
//   validate_session_result → chain + CAS check (replay precondition)

use crate::content_store::ContentRef;
use crate::core_api_facade::CoreApiFacade;
use crate::error::{LoomError, LoomErrorCode};
use crate::manifest_writer::{ManifestEntry, SessionId};
use crate::replay_engine::{DiffOpts, ReplayOpts};

impl CoreApiFacade {
    /// Replay a closed session. Returns the new replay session_id string.
    pub fn replay_session_to_id(&self, session_id: &str) -> Result<String, LoomError> {
        // Reject path-traversal session_ids before joining onto
        // sessions_root; otherwise `sessions_root.join("../...")`
        // resolves to a file outside the session-data directory.
        // Treat invalid IDs as not-found at the wire layer (consistent
        // with the rest of the surface).
        if !loom_shared::types::is_valid_session_id(session_id) {
            return Err(LoomError::new(
                LoomErrorCode::SessionNotFound,
                format!("session {session_id} does not exist"),
            ));
        }
        // Pre-flight: surface a typed `SessionNotFound` when the source
        // session directory is missing, instead of letting the generic
        // io::Error → LoomErrorCode::Io conversion bubble up as
        // `internal_error: session.replay failed`.
        let wal_path = self.sessions_root.join(session_id).join("manifest.wal");
        if !wal_path.exists() {
            return Err(LoomError::new(
                LoomErrorCode::SessionNotFound,
                format!("session {session_id} does not exist"),
            ));
        }
        let new_id = self
            .replay_engine
            .replay(SessionId(session_id.to_string()), ReplayOpts::default())?;
        Ok(new_id.0)
    }

    /// Diff two sessions; returns the loom-core DiffReport as a `serde_json::Value`.
    pub fn diff_sessions_json(
        &self,
        a: &str,
        b: &str,
        include_screenshots: bool,
    ) -> Result<serde_json::Value, LoomError> {
        // Path-traversal gate: reject malformed session_ids before
        // joining onto sessions_root.
        for sid in [a, b] {
            if !loom_shared::types::is_valid_session_id(sid) {
                return Err(LoomError::new(
                    LoomErrorCode::SessionNotFound,
                    format!("session {sid} does not exist"),
                ));
            }
        }
        // Pre-flight: surface SessionNotFound for missing source/target
        // instead of leaking a generic Io error.
        for sid in [a, b] {
            let wal = self.sessions_root.join(sid).join("manifest.wal");
            if !wal.exists() {
                return Err(LoomError::new(
                    LoomErrorCode::SessionNotFound,
                    format!("session {sid} does not exist"),
                ));
            }
        }
        let report = self.replay_engine.diff(
            SessionId(a.to_string()),
            SessionId(b.to_string()),
            DiffOpts {
                exclude_screenshots: !include_screenshots,
                include_audit_entries: false,
            },
        )?;
        serde_json::to_value(&report).map_err(|e| LoomError::internal(e.to_string()))
    }

    /// Inspect a session WAL up to (and including) `at_action`. Read-only.
    pub fn inspect_session_json(
        &self,
        session_id: &str,
        at_action: Option<u64>,
    ) -> Result<serde_json::Value, LoomError> {
        // Path-traversal gate: reject malformed session_ids before
        // joining onto sessions_root, so the read-only inspect path
        // can't be used to exfiltrate arbitrary file content as
        // receipt entries.
        if !loom_shared::types::is_valid_session_id(session_id) {
            return Ok(serde_json::json!({
                "session_id": session_id,
                "at_action": at_action,
                "action_count": 0,
                "entries": [],
            }));
        }
        let wal_path = self.sessions_root.join(session_id).join("manifest.wal");
        let content = match std::fs::read_to_string(&wal_path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(serde_json::json!({
                    "session_id": session_id,
                    "at_action": at_action,
                    "action_count": 0,
                    "entries": [],
                }));
            }
            Err(e) => return Err(LoomError::from(e)),
            Ok(c) => c,
        };

        let mut entries: Vec<serde_json::Value> = Vec::new();
        for line in content.lines() {
            if line.is_empty() {
                continue;
            }
            let entry: ManifestEntry = serde_json::from_str(line)
                .map_err(|e| LoomError::new(LoomErrorCode::ManifestCorrupt, e.to_string()))?;
            if let ManifestEntry::ActionReceipt {
                action_id,
                emitted_at_ms,
                receipt_canonical_bytes,
                ..
            } = entry
            {
                if at_action.is_none_or(|max| action_id <= max) {
                    let receipt: serde_json::Value =
                        serde_json::from_slice(&receipt_canonical_bytes)
                            .unwrap_or(serde_json::Value::Null);
                    entries.push(serde_json::json!({
                        "action_id": action_id,
                        "emitted_at_ms": emitted_at_ms,
                        "receipt": receipt,
                    }));
                }
            }
        }

        Ok(serde_json::json!({
            "session_id": session_id,
            "at_action": at_action,
            "action_count": entries.len(),
            "entries": entries,
        }))
    }

    /// Validate hash chain integrity + blob presence.
    ///
    /// `passed` covers integrity only; `replayable` additionally applies the
    /// replay-refusal checks (crashed/aborted source, `--no-determinism`
    /// recording) via the helpers shared with `ReplayEngine::replay`, so
    /// PASS ≠ replayable.
    pub fn validate_session_result(
        &self,
        session_id: &str,
    ) -> Result<crate::replay_engine::ValidationResult, LoomError> {
        // Path-traversal gate.
        if !loom_shared::types::is_valid_session_id(session_id) {
            return Err(LoomError::new(
                LoomErrorCode::SessionNotFound,
                format!("session {session_id} does not exist"),
            ));
        }
        // Pre-flight: when the session directory is missing, return a
        // typed SessionNotFound instead of letting the manifest-writer's
        // io::Error bubble up as the cryptic "chain: No such file or
        // directory (os error 2)" reason.
        let wal_path = self.sessions_root.join(session_id).join("manifest.wal");
        if !wal_path.exists() {
            return Err(LoomError::new(
                LoomErrorCode::SessionNotFound,
                format!("session {session_id} does not exist"),
            ));
        }

        let mut reasons = Vec::new();

        // 1. Hash chain check
        if let Err(e) = self
            .manifest_writer
            .validate(SessionId(session_id.to_string()))
        {
            reasons.push(format!("chain: {}", e.message));
        }

        // 2. Blob presence check (reuse `wal_path` from the
        // SessionNotFound pre-flight above; we know it exists here).
        // Tolerant per-line parse, matching the blob scan below — a torn
        // line must not turn validate into a hard error.
        let mut determinism_refusal = None;
        if let Ok(content) = std::fs::read_to_string(&wal_path) {
            let entries: Vec<ManifestEntry> = content
                .lines()
                .filter(|l| !l.is_empty())
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect();
            determinism_refusal = crate::replay_engine::non_deterministic_refusal(
                &entries,
                &SessionId(session_id.to_string()),
            );
        }
        // Walk the REAL production blob-ref fields via the shared
        // `collect_content_refs` (audit 2026-06-10, F32: this previously read
        // only the phantom `content_refs` array no production receipt carries,
        // so blob-presence validation silently passed for real sessions).
        if let Ok(content) = std::fs::read_to_string(&wal_path) {
            for line in content.lines() {
                if line.is_empty() {
                    continue;
                }
                if let Ok(ManifestEntry::ActionReceipt {
                    receipt_canonical_bytes,
                    ..
                }) = serde_json::from_str::<ManifestEntry>(line)
                {
                    for (sha256, kind) in
                        crate::replay_engine::collect_content_refs(&receipt_canonical_bytes)
                    {
                        if kind != "screenshot" && !sha256.is_empty() {
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

        // 3. Replayability verdict — mirrors `replay()`'s refusal order
        // (unclean source, then determinism), then folds in integrity:
        // a failed chain/blob check is refused by replay's pre-flight too.
        let refusal = crate::replay_engine::unclean_source_refusal(
            &self.sessions_root,
            &SessionId(session_id.to_string()),
        )
        .or(determinism_refusal)
        .map(|e| e.message);
        let passed = reasons.is_empty();
        let (replayable, not_replayable_reason) = match refusal {
            Some(reason) => (false, Some(reason)),
            None if !passed => (false, Some("validation failed (see reasons)".to_string())),
            None => (true, None),
        };

        Ok(crate::replay_engine::ValidationResult {
            session_id: session_id.to_string(),
            passed,
            reasons,
            replayable,
            not_replayable_reason,
        })
    }
}
