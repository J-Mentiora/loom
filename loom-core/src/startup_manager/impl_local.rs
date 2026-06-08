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
use crate::startup_manager::{FailedSession, QuarantineOutcome, RecoveryReport, StartupManager};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
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

/// Reject any directory name that isn't a plain single-component session id
/// before it can be used to build an `fs::rename` path. `read_dir` never yields
/// `.`/`..` or separators, so this can only fail on a hand-crafted name — but a
/// dir MOVE is exactly where a path-traversal name would matter, so guard it.
fn is_safe_session_dirname(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\'])
}

impl StartupManager {
    pub fn perform_recovery_sweep(&self) -> Result<RecoveryReport, LoomError> {
        let orphans = self.sweep_orphan_tmpfiles()?;
        // Quarantine corrupt-WAL orphans BEFORE the manifest sweep. A torn final
        // write breaks the hash chain; it cannot be reconciled by appending a
        // RuntimeCrash marker (that would chain over garbage and break replay-
        // equality — NFR-DET-01), so we move the dir aside instead. Doing it
        // first means `sweep_manifests` only ever sees healthy sessions. The
        // skip set is empty: the startup sweep runs before any RPC traffic, so
        // no session is live in memory yet.
        let quarantine = self.quarantine_corrupt_sessions(false, &HashSet::new())?;
        let (recovered, crashed, mut failed) = self.sweep_manifests()?;
        failed.extend(quarantine.failed);
        Ok(RecoveryReport {
            sessions_recovered: recovered,
            sessions_crashed: crashed,
            orphan_tmpfiles_removed: orphans,
            sessions_quarantined: quarantine.quarantined.len() as u64,
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
        // If we'd only spawn one worker anyway (constrained sandbox
        // where `available_parallelism` returned 1, or a corpus
        // barely above the parallel threshold), fall back to the
        // sequential path so we don't pay `thread::scope`'s setup
        // cost just to run one thread.
        if workers == 1 {
            return Ok(self.sweep_sequential(candidates));
        }
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

    /// Quarantine destination root — a sibling of `sessions_root` so the disk
    /// scanner (and thus the concurrency cap, which counts the on-disk active
    /// set) no longer sees the moved dirs. Non-destructive: corrupt sessions are
    /// preserved here for forensics, never deleted.
    fn quarantine_root(&self) -> PathBuf {
        self.sessions_root
            .parent()
            .map(|p| p.join("quarantine"))
            .unwrap_or_else(|| self.sessions_root.join(".quarantine"))
    }

    /// True when a session's on-disk WAL is a corrupt orphan: its hash chain
    /// fails to validate (or a line is unparseable — a torn write) AND it has
    /// no terminal marker (so it still reads as "active" and consumes a cap
    /// slot). A corrupt session that already has a terminal marker is not
    /// eating a slot, so we leave it alone (minimal, targeted).
    fn is_corrupt_orphan(&self, session_id: &SessionId) -> bool {
        let wal_path = self.sessions_root.join(&session_id.0).join("manifest.wal");
        let content = match std::fs::read_to_string(&wal_path) {
            Ok(c) => c,
            Err(_) => return false,
        };
        let mut has_terminal = false;
        let mut had_parse_error = false;
        for line in content.lines() {
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<ManifestEntry>(line) {
                Ok(ManifestEntry::SessionTerminal { .. } | ManifestEntry::RuntimeCrash { .. }) => {
                    has_terminal = true;
                }
                Ok(_) => {}
                Err(_) => had_parse_error = true,
            }
        }
        if has_terminal {
            return false;
        }
        // A torn line (parse error) OR a clean-parse-but-broken chain both mean
        // the WAL is unrecoverable in place.
        had_parse_error || self.manifest_writer.validate(session_id.clone()).is_err()
    }

    /// Move corrupt-WAL orphans out of `sessions_root` and into the quarantine
    /// root. Shared by the startup sweep (`skip` empty) and the on-demand
    /// `session.reap` operator command (`skip` = live in-memory session ids, so
    /// a session that is merely mid-WAL-write is never mistaken for corrupt).
    /// `dry_run` previews the candidates without touching the filesystem.
    pub fn quarantine_corrupt_sessions(
        &self,
        dry_run: bool,
        skip: &HashSet<SessionId>,
    ) -> Result<QuarantineOutcome, LoomError> {
        let qroot = self.quarantine_root();
        let mut outcome = QuarantineOutcome {
            dry_run,
            quarantine_dir: Some(qroot.clone()),
            ..Default::default()
        };

        let dir = match std::fs::read_dir(&self.sessions_root) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(outcome),
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
            // Defense-in-depth: `read_dir` only ever yields single path
            // components (never `.`/`..`/separators), but a quarantine MOVES a
            // directory by name, so refuse anything that isn't a plain session
            // dir name before it can reach an `fs::rename` path join.
            if !is_safe_session_dirname(&session_id.0) {
                continue;
            }
            if !path.join("manifest.wal").exists() {
                continue;
            }
            if skip.contains(&session_id) {
                outcome.skipped_live += 1;
                continue;
            }
            if !self.is_corrupt_orphan(&session_id) {
                continue;
            }
            if dry_run {
                outcome.quarantined.push(session_id);
                continue;
            }
            match self.quarantine_one(&qroot, &session_id) {
                Ok(()) => outcome.quarantined.push(session_id),
                Err(e) => outcome.failed.push(FailedSession {
                    session_id,
                    error_code: "quarantine_error".to_string(),
                    details: e.to_string(),
                }),
            }
        }
        Ok(outcome)
    }

    /// Move one corrupt session dir into the quarantine root via an atomic
    /// same-filesystem rename. A cross-device rename (EXDEV) or a pre-existing
    /// destination returns an error so the caller records it and leaves the
    /// session in place — never a silent drop, never a destructive copy+remove.
    fn quarantine_one(&self, qroot: &Path, session_id: &SessionId) -> Result<(), LoomError> {
        std::fs::create_dir_all(qroot)?;
        // Quarantined dirs hold the same (possibly sensitive) session data as
        // sessions_root, so keep the holding pen owner-only (0700) on unix.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(qroot, std::fs::Permissions::from_mode(0o700));
        }
        let src = self.sessions_root.join(&session_id.0);
        let dest = qroot.join(&session_id.0);
        if dest.exists() {
            return Err(LoomError::new(
                LoomErrorCode::Io,
                format!("quarantine destination already exists: {}", dest.display()),
            ));
        }
        std::fs::rename(&src, &dest)?;
        Ok(())
    }
}
