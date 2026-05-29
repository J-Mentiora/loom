// StartupManager implementation — crash-recovery sweeps.
//
// CAS orphan sweep — deletes NamedTempFile artifacts left by
//   interrupted put() calls. A file is an orphan if its reconstructed address
//   (parent2 + parent1 + filename) is not a 64-char lowercase hex string.
//
// Manifest sweep — appends a RuntimeCrash receipt and
//   checkpoints manifest.jsonl for every session whose last WAL entry has no
//   terminal (SessionTerminal or RuntimeCrash). Per-session isolation: one
//   failure does not block other sessions.

use crate::error::{LoomError, LoomErrorCode};
use crate::manifest_writer::{ManifestEntry, SessionId};
use crate::startup_manager::{FailedSession, RecoveryReport, StartupManager};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use walkdir::WalkDir;

/// v0.9.7 follow-up — cap parallel workers for the manifest sweep.
/// Larger fan-out helps for IO-bound work over hundreds of session
/// dirs, but more than ~16 workers stops paying for itself on typical
/// SSDs and starts contending on the manifest_writer's internal locks.
const SWEEP_MAX_WORKERS: usize = 16;

/// Per-worker minimum batch — sessions are cheap individually
/// (a few file reads + a JCS-canonical hash chain validation) so
/// spinning up a thread per session loses to the thread setup
/// overhead. Keep workers fed at least this many.
const SWEEP_MIN_BATCH: usize = 4;

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
        let dir = match std::fs::read_dir(&self.sessions_root) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((0, 0, vec![]));
            }
            Err(e) => return Err(LoomError::from(e)),
        };

        // Collect candidate session IDs first so the parallel fan-out
        // gets a deterministic, indexable input. Filtering is done up
        // front so each worker only sees real work — no wasted thread
        // wake-ups for sessions whose WAL was already truncated.
        let mut candidates: Vec<SessionId> = Vec::new();
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
            if !path.join("manifest.wal").exists() {
                continue;
            }
            candidates.push(session_id);
        }

        if candidates.is_empty() {
            return Ok((0, 0, vec![]));
        }

        // For small corpora the single-threaded fast path skips
        // thread::scope's overhead. The threshold is twice the min
        // batch — below that, the fan-out wouldn't even produce a
        // second worker.
        if candidates.len() < SWEEP_MIN_BATCH * 2 {
            return Ok(self.sweep_sequential(&candidates));
        }

        self.sweep_parallel(&candidates)
    }

    /// Single-threaded sweep — used for small corpora where parallel
    /// fan-out doesn't pay back the std::thread::scope overhead.
    fn sweep_sequential(&self, candidates: &[SessionId]) -> (u64, u64, Vec<FailedSession>) {
        let mut recovered = 0u64;
        let mut crashed = 0u64;
        let mut failed: Vec<FailedSession> = Vec::new();
        for session_id in candidates {
            match self.process_session(session_id.clone()) {
                Ok(true) => crashed += 1,
                Ok(false) => recovered += 1,
                Err(e) => failed.push(FailedSession {
                    session_id: session_id.clone(),
                    error_code: "sweep_error".to_string(),
                    details: e.to_string(),
                }),
            }
        }
        (recovered, crashed, failed)
    }

    /// Parallel sweep — partition candidates across N workers and
    /// run `process_session` in parallel. Per-session isolation is
    /// already a design property (each session has its own WAL +
    /// manifest_writer mutates only that session's dir), so the
    /// concurrent processing is safe.
    ///
    /// The recovered/crashed/failed counters are aggregated across
    /// worker threads via atomics + a single Mutex on the failed
    /// list — contention is low because failed entries are rare and
    /// the happy path only touches the atomics.
    fn sweep_parallel(
        &self,
        candidates: &[SessionId],
    ) -> Result<(u64, u64, Vec<FailedSession>), LoomError> {
        let worker_target = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(SWEEP_MAX_WORKERS);
        // Also cap workers so each one gets at least SWEEP_MIN_BATCH
        // sessions — fewer-but-busier threads beat lots-of-idle ones.
        let workers = worker_target
            .min(candidates.len().div_ceil(SWEEP_MIN_BATCH))
            .max(1);
        let chunk_size = candidates.len().div_ceil(workers);

        let recovered = AtomicU64::new(0);
        let crashed = AtomicU64::new(0);
        let failed: Mutex<Vec<FailedSession>> = Mutex::new(Vec::new());

        std::thread::scope(|scope| {
            for chunk in candidates.chunks(chunk_size) {
                let recovered_ref = &recovered;
                let crashed_ref = &crashed;
                let failed_ref = &failed;
                let me = self;
                scope.spawn(move || {
                    for session_id in chunk {
                        match me.process_session(session_id.clone()) {
                            Ok(true) => {
                                crashed_ref.fetch_add(1, Ordering::Relaxed);
                            }
                            Ok(false) => {
                                recovered_ref.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(e) => {
                                // Mutex is only taken on the rare
                                // failure path — happy-path threads
                                // never block on each other.
                                if let Ok(mut g) = failed_ref.lock() {
                                    g.push(FailedSession {
                                        session_id: session_id.clone(),
                                        error_code: "sweep_error".to_string(),
                                        details: e.to_string(),
                                    });
                                }
                            }
                        }
                    }
                });
            }
        });

        Ok((
            recovered.load(Ordering::Relaxed),
            crashed.load(Ordering::Relaxed),
            failed.into_inner().unwrap_or_default(),
        ))
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
            let entry: ManifestEntry = serde_json::from_str(line)
                .map_err(|e| LoomError::new(LoomErrorCode::ManifestCorrupt, e.to_string()))?;
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
