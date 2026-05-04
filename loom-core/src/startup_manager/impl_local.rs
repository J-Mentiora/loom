// StartupManager implementation — crash-recovery sweeps.
//
// AC-NFR-REL-02.1: CAS orphan sweep — deletes NamedTempFile artifacts left by
//   interrupted put() calls. A file is an orphan if its reconstructed address
//   (parent2 + parent1 + filename) is not a 64-char lowercase hex string.
//
// AC-NFR-REL-03.1: Manifest sweep — appends a RuntimeCrash receipt and
//   checkpoints manifest.jsonl for every session whose last WAL entry has no
//   terminal (SessionTerminal or RuntimeCrash). Per-session isolation: one
//   failure does not block other sessions.

use crate::error::{LoomError, LoomErrorCode};
use crate::manifest_writer::{ManifestEntry, SessionId};
use crate::startup_manager::{FailedSession, RecoveryReport, StartupManager};
use walkdir::WalkDir;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Return true if `s` is a 64-character lowercase hex string.
fn is_valid_cas_address(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
}

impl StartupManager {
    pub fn perform_recovery_sweep(&self) -> Result<RecoveryReport, LoomError> {
        let orphans = self.sweep_orphan_tmpfiles()?;
        let (recovered, crashed, failed) = self.sweep_manifests()?;
        Ok(RecoveryReport {
            sessions_recovered: recovered,
            sessions_crashed: crashed,
            orphan_tmpfiles_removed: orphans,
            failed_sessions: failed,
        })
    }

    pub fn sweep_orphan_tmpfiles(&self) -> Result<u64, LoomError> {
        let mut removed = 0u64;

        for entry in WalkDir::new(&self.cas_root)
            .min_depth(3)
            .max_depth(3)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let path = entry.path();

            // Reconstruct the full address from the 3-level path components.
            // Layout: cas_root/<aa>/<bb>/<rest> where aabbrest = 64-char hex.
            let rel = match path.strip_prefix(&self.cas_root) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let parts: Vec<_> = rel.components().collect();
            if parts.len() != 3 {
                continue;
            }

            let p0 = parts[0].as_os_str().to_str().unwrap_or("");
            let p1 = parts[1].as_os_str().to_str().unwrap_or("");
            let p2 = parts[2].as_os_str().to_str().unwrap_or("");
            let address = format!("{p0}{p1}{p2}");

            if !is_valid_cas_address(&address) {
                let _ = std::fs::remove_file(path);
                removed += 1;
            }
        }

        Ok(removed)
    }

    pub fn sweep_manifests(&self) -> Result<(u64, u64, Vec<FailedSession>), LoomError> {
        let mut recovered = 0u64;
        let mut crashed = 0u64;
        let mut failed: Vec<FailedSession> = Vec::new();

        let dir = match std::fs::read_dir(&self.sessions_root) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((0, 0, vec![]));
            }
            Err(e) => return Err(LoomError::from(e)),
        };

        for entry in dir {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let session_id = match path.file_name().and_then(|n| n.to_str()) {
                Some(s) => SessionId(s.to_string()),
                None => continue,
            };

            let wal_path = path.join("manifest.wal");
            if !wal_path.exists() {
                continue;
            }

            match self.process_session(session_id.clone()) {
                Ok(was_crashed) => {
                    if was_crashed {
                        crashed += 1;
                    } else {
                        recovered += 1;
                    }
                }
                Err(e) => {
                    failed.push(FailedSession {
                        session_id,
                        error_code: "sweep_error".to_string(),
                        details: e.to_string(),
                    });
                }
            }
        }

        Ok((recovered, crashed, failed))
    }

    /// Process one session. Returns Ok(true) if a RuntimeCrash was appended
    /// (orphaned active session), Ok(false) if already terminal.
    fn process_session(&self, session_id: SessionId) -> Result<bool, LoomError> {
        let wal_path = self.sessions_root.join(&session_id.0).join("manifest.wal");
        let content = std::fs::read_to_string(&wal_path)?;

        let mut has_terminal = false;
        let mut last_action_id: u64 = 0;

        for line in content.lines() {
            if line.is_empty() {
                continue;
            }
            let entry: ManifestEntry = serde_json::from_str(line).map_err(|e| {
                LoomError::new(LoomErrorCode::ManifestCorrupt, e.to_string())
            })?;
            match &entry {
                ManifestEntry::ActionReceipt { action_id, .. } => {
                    last_action_id = *action_id;
                }
                ManifestEntry::SessionTerminal { .. } | ManifestEntry::RuntimeCrash { .. } => {
                    has_terminal = true;
                }
                _ => {}
            }
        }

        // Validate hash chain; integrity failure → add to failed_sessions.
        self.manifest_writer.validate(session_id.clone())?;

        if has_terminal {
            return Ok(false);
        }

        // Orphaned active session — append RuntimeCrash.
        self.manifest_writer.append(
            session_id.clone(),
            ManifestEntry::RuntimeCrash {
                last_completed_action_id: last_action_id,
                emitted_at_ms: now_ms(),
                prev_hash: String::new(), // overwritten by append() via set_prev_hash
            },
        )?;

        // Checkpoint to manifest.jsonl.
        self.manifest_writer.checkpoint(session_id)?;

        Ok(true)
    }
}
