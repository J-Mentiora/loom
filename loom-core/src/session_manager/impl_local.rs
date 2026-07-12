// LocalSessionManager implementation — session lifecycle FSM.
// Public surface: create, get, close, profile-immutable guard,
//                 abort (dual-signal), abort_all (SIGTERM path).

use crate::budget_enforcer::{ResourceKind, SessionCounters};
use crate::error::LoomError;
use crate::manifest_writer::{ManifestEntry, SessionId};
use crate::session_manager::session_manager::{
    AbortReason, LocalSessionManager, Session, SessionActivity, SessionCreateOpts, SessionError,
    SessionStatus,
};
use crate::session_scope::SessionScope;
use loom_shared::types::{EpochMs, Seed};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use ulid::Ulid;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl LocalSessionManager {
    /// Create a new session.
    /// Critical path: ULID + ensure_dir + WAL header + fsync + optional task.
    pub fn create(&self, opts: SessionCreateOpts) -> Result<SessionId, LoomError> {
        let limits = opts.limits;
        let started_at_ms_override = opts.started_at_ms_override;
        let is_replay = opts.replay_of.is_some();
        let capture_policy = opts.capture_policy.clone();
        let no_blocklist = opts.no_blocklist;
        let no_determinism = opts.no_determinism;
        let record_screencast = opts.record_screencast;
        let audio = opts.audio;
        let profile = opts.profile.clone();

        // The Option<u64> → Seed collapse happens HERE — exactly once.
        // Downstream layers carry concrete Seed(u64).
        let seed = Seed(opts.seed.unwrap_or(self.default_seed));
        // Per-session epoch_ms; replay path uses started_at_ms_override
        // for hash-chain bit-equality, otherwise current time.
        let epoch_ms = EpochMs(started_at_ms_override.unwrap_or_else(now_ms));

        let id = SessionId(Ulid::new().to_string().to_lowercase());

        // Under safe profile, create a session-scoped downloads
        // directory. Chromium's
        // `Browser.setDownloadBehavior(allowAndName, downloadPath=<dir>)`
        // confines all downloads to this dir. Created with mode 0700 so
        // other local users can't peek at downloaded artifacts.
        let downloads_dir: Option<PathBuf> = if profile == "safe" {
            let dir = self.sessions_root.join(&id.0).join("downloads");
            std::fs::create_dir_all(&dir).map_err(LoomError::from)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                // Best-effort: chmod failure is non-fatal — Chromium still
                // confines to the dir; the only risk is permissive perms.
                // Surfaced via tracing so an operator notices if the
                // process umask blocks 0700 and downloads land in a
                // world-readable dir.
                if let Err(e) =
                    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                {
                    tracing::warn!(
                        path = %dir.display(),
                        error = %e,
                        "chmod 0o700 failed on session downloads dir; \
                         downloaded artifacts may be world-readable"
                    );
                }
            }
            Some(dir)
        } else {
            None
        };

        // Open manifest — creates `sessions/<ulid>/manifest.wal` + Header entry.
        // Pass limits so the Header records budget overrides.
        // started_at_ms_override is set on the replay path so
        // the Header's timestamp matches the source session — the prev_hash
        // chain hashes over the Header's canonical bytes.
        // capture_policy is persisted into the Header.
        let handle = self.manifest_writer.open_manifest_with_started_at(
            id.clone(),
            limits,
            started_at_ms_override,
            capture_policy.clone(),
            // settle-capture (D3): record the resolved seed so replay can
            // reconstruct it. On the replay path `opts.seed` is the recorded
            // source seed, so this records the same value round-trip.
            Some(seed.0),
            // settle-capture (4b): record whether determinism was ON so the
            // replay path can REFUSE non-deterministic sessions (they can never
            // be replay-equal). `true` for the default deterministic session.
            !no_determinism,
        )?;

        // Per-session determinism harness, seeded with the RESOLVED seed
        // (explicit --seed N, else default_seed). The same resolved value
        // was recorded in the manifest Header above, and the replay engine
        // re-creates its session with that Header seed — so the harness
        // seed is authoritative and identical on record and replay, while
        // concurrent sessions own disjoint ChaCha20 streams + virtual
        // clocks (no cross-session interleaving; --seed actually isolates).
        let determinism = Arc::new(crate::determinism_harness::DeterminismHarness::new(
            seed.0,
            Arc::clone(&self.manifest_writer),
        ));
        let tape_writer = determinism.new_tape_writer();
        let counters = Arc::new(SessionCounters::default());
        let scope = SessionScope::new();
        let session = Arc::new(Session {
            id: id.clone(),
            status: parking_lot::Mutex::new(SessionStatus::Active),
            abort_flag: Arc::new(AtomicBool::new(false)),
            abort_notify: Arc::new(tokio::sync::Notify::new()),
            writer: Arc::new(handle),
            counters: Arc::clone(&counters),
            tape_writer: Arc::new(tokio::sync::Mutex::new(tape_writer)),
            scope: Arc::clone(&scope),
            last_activity_ms: AtomicU64::new(epoch_ms.0),
            in_flight_actions: AtomicU32::new(0),
            dispatch_slot: parking_lot::Mutex::new(()),
            next_action_id: AtomicU64::new(0),
            kill_reason: Arc::new(parking_lot::Mutex::new(None)),
            capture_policy,
            no_blocklist,
            no_determinism,
            record_screencast,
            audio,
            seed,
            determinism,
            epoch_ms,
            profile,
            downloads_dir,
        });

        self.sessions.insert(id.clone(), Arc::clone(&session));

        // Bind the session's vault determinism context (NFR-DET-01): grant
        // ids + audit ts_ticks derive from the session seed, keeping vault
        // audit canonical_bytes cross-run hash-equal.
        self.vault.begin_session(&id, seed.0);

        // Wire budget enforcement.
        // Replay sessions skip enforcement — the manifest header already
        // pins the original budget; rerunning the wall-clock timer would
        // poison determinism.
        if !is_replay {
            if let Some(limits) = limits {
                let kill_callback = self.kill_callback_for(id.clone());
                self.budget_enforcer.register_session(
                    id.clone(),
                    Arc::clone(&counters),
                    limits,
                    kill_callback,
                );

                // Spawn the per-session wall-clock timer if a positive
                // budget is set. Zero/None → no timer (caller opts out).
                // The timer runs as a SessionScope child so close/abort can
                // signal it via the scope's cancellation token — no more
                // separate JoinHandle bookkeeping or fire-and-forget abort.
                if limits.session_walltime_ms > 0 {
                    let enforcer = Arc::clone(&self.budget_enforcer);
                    let timer_id = id.clone();
                    let budget_ms = limits.session_walltime_ms;
                    let cancel = scope.child_token();
                    scope.spawn(async move {
                        tokio::select! {
                            // Session closed/aborted before budget expired —
                            // exit without firing the kill callback.
                            _ = cancel.cancelled() => {}
                            // Budget expired — account(Walltime, full_budget)
                            // trips the threshold gate inside
                            // LocalBudgetEnforcer::account, which fires the
                            // kill callback exactly once (idempotent via
                            // SessionBudgetEntry::killed AtomicBool).
                            _ = tokio::time::sleep(Duration::from_millis(budget_ms)) => {
                                let _ = enforcer.account(
                                    timer_id,
                                    ResourceKind::Walltime,
                                    budget_ms,
                                );
                            }
                        }
                    });
                }
            }
        }

        Ok(id)
    }

    /// Look up a session (any status). Returns SessionNotFound for unknown IDs.
    pub fn get(&self, id: SessionId) -> Result<Arc<Session>, LoomError> {
        self.sessions.get(&id).map(|r| r.clone()).ok_or_else(|| {
            LoomError::from(SessionError::SessionUnknown {
                session_id: id.0.clone(),
            })
        })
    }

    /// Close a session: Active → Closed + SessionTerminal manifest entry.
    /// Subsequent calls on a closed session return SessionAlreadyClosed.
    pub fn close(&self, id: SessionId) -> Result<(), LoomError> {
        self.close_with_reason(id, "close")
    }

    /// Close a session with a specific terminal `reason` (e.g. `idle_ttl`, `zombie`) so the
    /// reaper's cause is captured deterministically in the manifest `SessionTerminal`. The
    /// reason is a fixed string per eviction kind, so the replay hash chain stays equal.
    pub fn close_with_reason(&self, id: SessionId, reason: &str) -> Result<(), LoomError> {
        match self.close_inner(id.clone(), reason, None)? {
            true => Ok(()),
            // None-guard path always returns true or errors; false is unreachable here.
            false => Err(LoomError::from(SessionError::SessionAlreadyClosed {
                session_id: id.0,
            })),
        }
    }

    /// Two-phase idle eviction (hardening A): under the SAME status lock that performs the
    /// Active→Closed transition, re-verify the session is still Active, still idle past
    /// `ttl_ms`, AND has no in-flight action — so a session is never evicted mid-action even
    /// if an action started between the reaper's decision and this call. Returns `Ok(true)`
    /// if it closed the session, `Ok(false)` if it was spared (no longer idle / now busy /
    /// already gone) — sparing is NOT an error for the sweep.
    pub fn evict_if_idle(
        &self,
        id: SessionId,
        ttl_ms: u64,
        now_ms: u64,
    ) -> Result<bool, LoomError> {
        self.close_inner(id, "idle_ttl", Some((ttl_ms, now_ms)))
    }

    /// Shared close body. `idle_guard = Some((ttl, now))` enables the two-phase re-check; in
    /// that mode a non-Active / busy / no-longer-idle session yields `Ok(false)` (spared)
    /// rather than an error. `idle_guard = None` is the unconditional close (returns an error
    /// only if the session was already not Active, matching the historic `close()` contract —
    /// surfaced by the `close_with_reason` wrapper).
    fn close_inner(
        &self,
        id: SessionId,
        reason: &str,
        idle_guard: Option<(u64, u64)>,
    ) -> Result<bool, LoomError> {
        let session = self.get(id.clone())?;

        {
            let mut status = session.status.lock();
            if *status != SessionStatus::Active {
                return match idle_guard {
                    Some(_) => Ok(false), // already gone — fine for the sweep
                    None => Err(LoomError::from(SessionError::SessionAlreadyClosed {
                        session_id: id.0.clone(),
                    })),
                };
            }
            if let Some((ttl_ms, now_ms)) = idle_guard {
                // Re-check idleness + in-flight UNDER the lock (hardening A). If an action
                // started or the session became active again in the decision→close gap,
                // spare it.
                let idle_for = now_ms.saturating_sub(session.last_activity());
                if session.in_flight() > 0 || idle_for < ttl_ms {
                    return Ok(false);
                }
            }
            *status = SessionStatus::Closed;
        }

        // Bounded terminal retention: record the transition so the oldest
        // terminal sessions are evicted from the in-memory table (see the
        // eviction-policy note on `LocalSessionManager::sessions`).
        self.note_terminal(id.clone());

        // Signal every scope-owned task (budget timer, shim-IPC tasks,
        // receipt spawns) to exit cooperatively. The actual drain (joining
        // them with a grace period) runs at the daemon bridge layer in
        // `close_session_raw` — keeping this synchronous preserves the
        // existing call signature across the codebase + tests + benchmarks.
        session.scope.cancel();
        self.budget_enforcer.unregister_session(id.clone());
        self.vault.end_session(&id);

        self.manifest_writer.append(
            id,
            ManifestEntry::SessionTerminal {
                action_id: 0,
                emitted_at_ms: now_ms(),
                reason: reason.to_string(),
                prev_hash: String::new(),
            },
        )?;

        Ok(true)
    }

    /// Activity snapshot of every session, for the idle/zombie reaper decisions. Pairs each
    /// id with its last-activity epoch-ms, in-flight count, and active flag.
    pub fn session_activity_snapshot(&self) -> Vec<SessionActivity> {
        self.sessions
            .iter()
            .map(|e| {
                let s = e.value();
                SessionActivity {
                    id: e.key().clone(),
                    last_activity_ms: s.last_activity(),
                    in_flight: s.in_flight(),
                    is_active: *s.status.lock() == SessionStatus::Active,
                }
            })
            .collect()
    }

    /// Age in seconds of the oldest Active session (by last-activity), or `None` if no
    /// Active sessions exist. Surfaced in `daemon.health` / `loom doctor`.
    pub fn oldest_active_age_secs(&self, now_ms: u64) -> Option<u64> {
        self.sessions
            .iter()
            .filter(|e| *e.value().status.lock() == SessionStatus::Active)
            .map(|e| now_ms.saturating_sub(e.value().last_activity()) / 1000)
            .max()
    }

    /// Abort a session: flip abort_flag + notify_one + Active → Aborted.
    /// Returns immediately after signaling (≤1s SLA for propagation).
    /// Appends SessionTerminal synchronously.
    pub fn abort(&self, id: SessionId, reason: AbortReason) -> Result<(), LoomError> {
        let session = self.get(id.clone())?;

        {
            let mut status = session.status.lock();
            if *status != SessionStatus::Active {
                return Err(LoomError::from(SessionError::SessionAlreadyClosed {
                    session_id: id.0.clone(),
                }));
            }
            // Signal the session task (and any host-fn polling the flag).
            // NOTE: kill_reason is intentionally left untouched — the
            // executor's abort arm reads None here and emits
            // ActionOutcome::Aborted (vs Trapped for a budget kill).
            session.abort_flag.store(true, Ordering::Release);
            session.abort_notify.notify_one();
            *status = SessionStatus::Aborted;
        }

        // Bounded terminal retention (see `LocalSessionManager::sessions`).
        self.note_terminal(id.clone());

        // Signal every scope-owned task (budget timer, shim-IPC tasks,
        // receipt spawns) to exit cooperatively. Actual drain happens at
        // the daemon bridge layer.
        session.scope.cancel();
        self.budget_enforcer.unregister_session(id.clone());
        self.vault.end_session(&id);

        self.manifest_writer.append(
            id,
            ManifestEntry::SessionTerminal {
                action_id: 0,
                emitted_at_ms: now_ms(),
                reason: reason.reason,
                prev_hash: String::new(),
            },
        )?;

        Ok(())
    }

    /// IDs of every session the daemon currently holds in memory. Used by
    /// `session.reap` as the skip set: a corrupt-looking WAL that belongs to a
    /// live session is just mid-write, not an abandoned orphan — never quarantine
    /// it. (Startup recovery doesn't need this; it runs before any session is
    /// created, so the live set is empty then.)
    pub fn live_session_ids(&self) -> std::collections::HashSet<SessionId> {
        self.sessions.iter().map(|e| e.key().clone()).collect()
    }

    /// Number of sessions currently `Active` in the in-memory FSM. O(live map),
    /// no disk I/O — this is the authoritative count for the daemon's
    /// concurrency cap: close/abort/budget-kill flip status under the session
    /// lock and immediately leave this count, and the startup recovery sweep
    /// stamps any on-disk "active" leftovers of a crashed daemon as
    /// RuntimeCrash before the daemon serves, so a fresh process correctly
    /// starts from zero.
    pub fn active_session_count(&self) -> usize {
        self.sessions
            .iter()
            .filter(|e| *e.value().status.lock() == SessionStatus::Active)
            .count()
    }

    /// Abort all Active sessions. Called by startup_manager on SIGTERM.
    pub fn abort_all(&self, reason: AbortReason) -> Result<(), LoomError> {
        let active_ids: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|e| *e.value().status.lock() == SessionStatus::Active)
            .map(|e| e.key().clone())
            .collect();

        for id in active_ids {
            let _ = self.abort(id, reason.clone());
        }

        Ok(())
    }
}
