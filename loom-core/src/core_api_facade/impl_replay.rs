// CoreApiFacade replay/diff/inspect/validate — bridges CoreFacadeBridge methods
// to loom-core internals.
//
// AC coverage:
//   AC-REPLAY-01.1 / AC-NFR-DET-01.1: replay_session_to_id → ReplayEngine::replay
//   AC-REPLAY-02.1-02.3: diff_sessions_json → ReplayEngine::diff
//   AC-REPLAY-03.1: inspect_session_json → WAL read (read-only)
//   AC-REPLAY-01.2 precondition: validate_session_result → chain + CAS check

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
        let wal_path = self
            .sessions_root
            .join(session_id)
            .join("manifest.wal");
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
        serde_json::to_value(&report)
            .map_err(|e| LoomError::internal(e.to_string()))
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

    /// Validate hash chain integrity + blob presence. Returns `(passed, reasons)`.
    pub fn validate_session_result(
        &self,
        session_id: &str,
    ) -> Result<(bool, Vec<String>), LoomError> {
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
        if let Err(e) = self.manifest_writer.validate(SessionId(session_id.to_string())) {
            reasons.push(format!("chain: {}", e.message));
        }

        // 2. Blob presence check (reuse `wal_path` from the
        // SessionNotFound pre-flight above; we know it exists here).
        if let Ok(content) = std::fs::read_to_string(&wal_path) {
            for line in content.lines() {
                if line.is_empty() {
                    continue;
                }
                if let Ok(ManifestEntry::ActionReceipt { receipt_canonical_bytes, .. }) =
                    serde_json::from_str::<ManifestEntry>(line)
                {
                    if let Ok(val) =
                        serde_json::from_slice::<serde_json::Value>(&receipt_canonical_bytes)
                    {
                        if let Some(refs) =
                            val.get("content_refs").and_then(|r| r.as_array())
                        {
                            for r in refs {
                                let sha256 = r
                                    .get("sha256")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let kind = r
                                    .get("kind")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                if kind != "screenshot" && !sha256.is_empty() {
                                    let cr =
                                        ContentRef { sha256: sha256.clone(), size_bytes: 0 };
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
            }
        }

        Ok((reasons.is_empty(), reasons))
    }
}
