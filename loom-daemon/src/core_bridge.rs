//! `CoreApiFacade` → `CoreFacadeBridge` bridge + per-session dispatch fence.
//!
//! Split out of `lib.rs` (large-file refactor), unchanged:
//!   * `ActionActivityGuard` — RAII guard bracketing one action dispatch
//!     (decrements in-flight + refreshes the idle clock on drop).
//!   * `acquire_dispatch_slot` — the per-session dispatch fence
//!     (`too_many_requests` while a prior dispatch still holds it).
//!   * `CoreBridge` + its `CoreFacadeBridge` impl — translates the
//!     `loom-core` / `loom-rpc` type vocabularies and routes vault calls
//!     to the `vault_bridge` submodule; spawns background shim teardown on
//!     session close/abort.

use crate::{map_loom_error, map_loom_error_full, max_concurrent_sessions, now_epoch_ms, reaper};
use loom_core::core_api_facade::{
    CoreApiFacade, ExportInfo as CoreExportInfo,
    PlaywrightImportResult as CorePlaywrightImportResult,
};
use loom_core::error::LoomError;
use loom_rpc::core_service_adapter::core_service_adapter::{
    AdapterError, CoreFacadeBridge, CreateSessionParams, ExportInfo, GrantInfo, GrantParams,
    PlaywrightImportInfo, VaultAddInfo, VaultAddParams, VaultDeleteSecretInfo,
    VaultDeleteSecretParams, VaultDiagnoseInfo, VaultListLabelsInfo, VaultListLabelsParams,
    VaultSetSecretInfo, VaultSetSecretParams,
};
use loom_rpc::host_service_adapter::host_service_adapter::AdapterError as HostAdapterError;
use std::sync::Arc;

/// RAII guard that decrements a session's in-flight action counter (and refreshes its idle
/// clock) when an action dispatch returns, no matter which path. Constructed AFTER
/// `Session::action_started`, so creation+drop bracket exactly one action.
pub(crate) struct ActionActivityGuard(pub(crate) Arc<loom_core::session_manager::Session>);

impl Drop for ActionActivityGuard {
    fn drop(&mut self) {
        self.0.action_finished(now_epoch_ms());
    }
}

/// Acquire the per-session dispatch fence, failing fast with a typed
/// `too_many_requests` when another surface-verb dispatch holds it.
///
/// The slot is held for the FULL blocking dispatch — including after the
/// RPC layer abandons the request on timeout/cancel (the spawn_blocking
/// work runs detached to completion). Holding-until-done is the fence:
/// a later action on the same session cannot start while the abandoned
/// work might still mutate browser/session state, and per-session WAL +
/// receipt order stays strictly dispatch-ordered (NFR-DET-01). Sequential
/// clients never see the busy error — the slot is released before the
/// previous response is written. Only pipelined same-session actions and
/// post-timeout/cancel races do, and `too_many_requests` carries
/// back-off-and-retry semantics in every client.
pub(crate) fn acquire_dispatch_slot(
    session: &loom_core::session_manager::Session,
) -> Result<parking_lot::MutexGuard<'_, ()>, HostAdapterError> {
    use loom_rpc::error_translator::error_translator::LoomErrorCode;
    session.dispatch_slot.try_lock().ok_or_else(|| {
        tracing::warn!(
            metric = "loom_daemon_session_dispatch_busy",
            session_id = %session.id.0,
            "rejecting action: a previous dispatch on this session is still running \
             (possibly abandoned by a timeout/cancel; it is fenced until it finishes)"
        );
        LoomErrorCode::TooManyRequests
    })
}

// ─── Bridge: CoreApiFacade → CoreFacadeBridge ───────────────────────────────

/// Wraps `Arc<CoreApiFacade>` and implements the `CoreFacadeBridge`
/// trait required by `CoreServiceAdapter`. Converts between the
/// `loom-core` and `loom-rpc` type vocabularies.
pub(crate) struct CoreBridge {
    pub(crate) core: Arc<CoreApiFacade>,
    /// Optional WasmHost handle. When present, `close_session_raw` and
    /// `abort_session_raw` spawn `host.shutdown_session(...)` into the
    /// bridge's `cleanup_tasks` JoinSet so any session-bound shim
    /// subprocesses (e.g. Chromium) get cooperatively torn down. None
    /// when modules haven't been compiled yet (matches StubHostBridge
    /// fallback).
    pub(crate) wasm_host: Option<Arc<loom_host::WasmHost>>,
    /// Tracks background `host.shutdown_session` spawns from
    /// `spawn_shim_teardown`. Previously a bare `tokio::spawn` here leaked
    /// `JoinHandle`s — when shim teardown stalled on SIGTERM grace,
    /// each session.close left a never-joined task behind; after a few
    /// sequential sessions the daemon's runtime saturated. Spawns now go
    /// into this `JoinSet`, reaped opportunistically on every close/abort.
    pub(crate) cleanup_tasks: Arc<std::sync::Mutex<tokio::task::JoinSet<()>>>,
}

impl CoreBridge {
    /// Background shim teardown for a session leaving the Active state, so
    /// the session-bound Chromium subprocess AND its ShimManager
    /// processes/states/configs entries get reclaimed. Shared by
    /// `close_session_raw` and `abort_session_raw`: abort legitimately skips
    /// the graceful in-session drain, but the shim must still be torn down —
    /// without this, an aborted session's browser kept running until the
    /// orphan-GC sweep aged it out and its ShimManager entries leaked
    /// forever. `host.shutdown_session` is idempotent, so overlapping with
    /// `session.kill`'s separate `shutdown_with_ceiling` path is safe.
    ///
    /// Tracked in `cleanup_tasks` so the JoinHandle isn't leaked. Completed
    /// cleanups are reaped BEFORE the fresh spawn so the JoinSet stays
    /// bounded across many close/abort calls (and the fresh task is always
    /// observable in `len()` right after this returns).
    fn spawn_shim_teardown(&self, session_id: &str) {
        let Some(host) = self.wasm_host.clone() else {
            return;
        };
        let sid = session_id.to_string();
        // `unwrap` is safe: the only way the mutex is poisoned is if a
        // previous holder panicked while spawning into the JoinSet,
        // which would have already crashed the process — there is no
        // recovery path that's better than propagating the panic.
        let mut set = self.cleanup_tasks.lock().unwrap();
        // Reap completed cleanups + count for visibility.
        let mut reaped = 0usize;
        while set.try_join_next().is_some() {
            reaped += 1;
        }
        set.spawn(async move {
            host.shutdown_session(&sid).await;
        });
        tracing::debug!(
            metric = "loom_daemon_close_cleanup_spawn",
            session_id = %session_id,
            pending = set.len(),
            reaped,
        );
    }
}

impl CoreFacadeBridge for CoreBridge {
    fn export_session_to_cas(
        &self,
        session_id: &str,
        format: &str,
    ) -> Result<ExportInfo, AdapterError> {
        let r: CoreExportInfo = self
            .core
            .export_session_to_cas(session_id, format)
            .map_err(|e| map_loom_error(&e))?;
        Ok(ExportInfo {
            session_id: r.session_id,
            format: r.format,
            artifact_ref: r.artifact_ref,
        })
    }

    fn get_export_bytes(&self, artifact_ref: &str) -> Result<Vec<u8>, AdapterError> {
        self.core
            .get_export_bytes(artifact_ref)
            .map_err(|e| map_loom_error(&e))
    }

    fn list_sessions_info(&self) -> Result<Vec<(String, String, u64)>, AdapterError> {
        self.core
            .list_sessions_info()
            .map_err(|e| map_loom_error(&e))
    }

    fn replay_session_to_id(&self, session_id: &str) -> Result<String, LoomError> {
        // Replay-refusal fidelity: pass the canonical `LoomError` through
        // UNTRANSLATED. Every code the replay path emits is already a
        // canonical wire code (`session_not_found`, `session_aborted`,
        // `manifest_corrupt`, `replay_missing_blob`, `not_replayable`,
        // `io`, `internal`), and `map_loom_error` would both coarsen the
        // code (e.g. ManifestCorrupt → StoreIntegrityFailed) and drop the
        // compiled-in refusal message on the floor.
        self.core.replay_session_to_id(session_id)
    }

    fn diff_sessions_json(
        &self,
        a: &str,
        b: &str,
        include_screenshots: bool,
    ) -> Result<serde_json::Value, AdapterError> {
        self.core
            .diff_sessions_json(a, b, include_screenshots)
            .map_err(|e| map_loom_error(&e))
    }

    fn inspect_session_json(
        &self,
        session_id: &str,
        at_action: Option<u64>,
    ) -> Result<serde_json::Value, AdapterError> {
        self.core
            .inspect_session_json(session_id, at_action)
            .map_err(|e| map_loom_error(&e))
    }

    fn validate_session_result(
        &self,
        session_id: &str,
    ) -> Result<loom_rpc::core_service_adapter::core_service_adapter::ValidationResult, AdapterError>
    {
        let r = self
            .core
            .validate_session_result(session_id)
            .map_err(|e| map_loom_error(&e))?;
        Ok(
            loom_rpc::core_service_adapter::core_service_adapter::ValidationResult {
                session_id: r.session_id,
                passed: r.passed,
                reasons: r.reasons,
                replayable: r.replayable,
                not_replayable_reason: r.not_replayable_reason,
            },
        )
    }

    fn import_playwright_from_bytes(
        &self,
        trace_bytes: &[u8],
    ) -> Result<PlaywrightImportInfo, AdapterError> {
        let r: CorePlaywrightImportResult = self
            .core
            .import_playwright_from_bytes(trace_bytes)
            .map_err(|e| map_loom_error(&e))?;
        Ok(PlaywrightImportInfo {
            session_id: r.session_id,
            action_count: r.action_count,
        })
    }

    fn create_session_raw(&self, params: CreateSessionParams) -> Result<(String, u64), LoomError> {
        // `params.network_mode` is intentionally unread: always `"live"`
        // (session_validation rejects everything else) and there is no
        // page-network record/replay engine — don't plumb it further
        // without building one. The remaining fields move out of `params`
        // below; the partial-move order is load-bearing — `params.budget` is
        // consumed first, then `params.profile` / `params.capture_policy` into
        // `SessionCreateOpts` (disjoint field moves out of the owned struct).
        use loom_core::budget_enforcer::BudgetLimits;
        use loom_core::error::LoomErrorCode;
        use loom_core::session_manager::SessionCreateOpts;

        // Concurrency cap: fail fast (retryable) when too many sessions are
        // already active, rather than spawning an unbounded number of chromium
        // shims. Counts the AUTHORITATIVE in-memory live-session set (no
        // separate counter to drift or leak — a closed/aborted/budget-killed
        // session flips status under its lock and immediately leaves the
        // count). Previously this re-read and JSON-parsed EVERY session WAL on
        // disk per create — O(total sessions × WAL bytes) of synchronous I/O on
        // a tokio worker, monotonically degrading create latency on a
        // long-running daemon. The in-memory FSM is at least as accurate:
        // sessions a crashed daemon left "active" on disk are stamped
        // RuntimeCrash by the startup recovery sweep before serving, and
        // corrupt-WAL orphans (torn write from a hard kill) were never counted
        // by either source — the disk scanner reports them "corrupt" and they
        // are reclaimed by quarantine / `session.reap`. Cap-hit → typed
        // `SessionCapExceeded` (wire `session_cap_exceeded`) carrying
        // `{active, cap, hint}` so the caller can tell "busy — back off and
        // retry when a slot frees (NOT a reconnect)" from "daemon is broken".
        //
        // Fail-open vs fail-closed on introspection errors: moot since the
        // FSM-count rework — `active_session_count()` is infallible (an
        // in-memory map walk; no I/O, no poisoning), so the old fail-open
        // branch ("could not read active sessions; allowing create", metric
        // `loom_daemon_cap_introspect_error`) no longer exists and the cap
        // cannot be silently overshot by a read error. If counting ever
        // becomes fallible again, the choice must be made deliberately here
        // and any failure logged at ERROR, not WARN.
        //
        // This is a best-effort, eventually-consistent resource valve, not a
        // hard security boundary (the daemon is single-user/local): a check-then-
        // create window means N concurrent creates at the boundary can transiently
        // overshoot by up to N-1, but the cap re-reads authoritative state every
        // call so it always self-corrects and can't be defeated long-term. A
        // strict atomic reservation would reintroduce the counter-leak-on-crash
        // problem the live-set design deliberately avoids — not worth it here.
        let cap = max_concurrent_sessions();
        let active = self.core.session_manager.active_session_count();
        if active >= cap {
            tracing::warn!(
                metric = "loom_daemon_cap_reject",
                active,
                cap,
                "session.create rejected: concurrent session cap reached"
            );
            return Err(LoomError::new(
                LoomErrorCode::SessionCapExceeded,
                format!(
                    "concurrent session cap reached ({active}/{cap}); close sessions or run \
                     `loom session reap` to free leaked slots, then retry"
                ),
            )
            .with_context(serde_json::json!({
                "active": active,
                "cap": cap,
                "hint": "close sessions or run `loom session reap`",
            })));
        }

        let limits: Option<BudgetLimits> = match params.budget {
            Some(value) => Some(serde_json::from_value(value).map_err(|e| {
                map_loom_error_full(&LoomError::new(
                    LoomErrorCode::InvalidArgument,
                    format!("invalid budget JSON: {e}"),
                ))
            })?),
            None => None,
        };
        let opts = SessionCreateOpts {
            agent_id: "rpc-client".to_string(),
            surface: "web".to_string(),
            seed: params.seed,
            limits,
            replay_of: None,
            // --clock-anchor pins the session clock via the same override the
            // replay path uses: started_at_ms_override → epoch_ms (impl_local.rs)
            // → CDP initialVirtualTime + Header started_at_ms + replay round-trip.
            // None → epoch falls back to wall-clock now_ms() (unchanged behavior).
            started_at_ms_override: params.clock_anchor,
            capture_policy: params.capture_policy,
            no_blocklist: params.no_blocklist,
            no_determinism: params.no_determinism,
            record_screencast: params.record_screencast,
            // `--profile` must reach the Session: the evaluate gate (B) and
            // download confinement (C) both branch on `Session.profile`.
            profile: params.profile,
        };
        if let Some(anchor) = params.clock_anchor {
            // Greppable signal that a cross-run clock anchor was applied (the
            // session's injected Date.now/performance.now epoch is pinned to this).
            tracing::info!(
                clock_anchor = anchor,
                "session create: clock anchor applied"
            );
        }
        let session_id = self
            .core
            .session_manager
            .create(opts)
            .map_err(|e| map_loom_error_full(&e))?;
        let created_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        Ok((session_id.0, created_at_ms))
    }

    fn close_session_raw(&self, session_id: &str) -> Result<(), AdapterError> {
        use loom_core::manifest_writer::SessionId;
        let result = self
            .core
            .session_manager
            .close(SessionId(session_id.to_string()))
            .map_err(|e| map_loom_error(&e));

        // Background shim teardown so any session-bound Chromium subprocess
        // gets cooperatively reaped. Spawned even when close errored (e.g.
        // already-closed): teardown is idempotent and a stale shim entry
        // must not survive a lost close/teardown race.
        self.spawn_shim_teardown(session_id);

        result
    }

    fn abort_session_raw(&self, session_id: &str, reason: &str) -> Result<(), AdapterError> {
        use loom_core::manifest_writer::SessionId;
        use loom_core::session_manager::AbortReason;
        let result = self
            .core
            .session_manager
            .abort(
                SessionId(session_id.to_string()),
                AbortReason {
                    reason: reason.to_string(),
                },
            )
            .map_err(|e| map_loom_error(&e));

        // Abort flips core state immediately (≤1s signal SLA) but the
        // session-bound chromium shim must STILL be reclaimed — mirroring
        // `close_session_raw`. Previously abort performed no shim teardown
        // at all: the browser ran on until orphan GC aged it out and the
        // ShimManager entries for `chromium:<sid>` leaked for the daemon's
        // lifetime.
        self.spawn_shim_teardown(session_id);

        result
    }

    // ── Vault bridge methods ────────────────────────────────────────────
    //
    // Each method translates between loom-rpc wire types
    // (`GrantParams`/`VaultAddParams`/`GrantInfo`) and loom-core domain
    // types (`GrantOpts`/`AddCredentialOpts`/`GrantSnapshot`). Errors
    // route through `map_loom_error` so the wire envelope carries the
    // distinct `vault_grant_revoked`/`vault_grant_expired`/`vault_rejection`
    // codes .

    fn vault_grant(&self, p: GrantParams) -> Result<GrantInfo, AdapterError> {
        crate::vault_bridge::vault_grant(&self.core, p)
    }

    fn vault_revoke(&self, grant_id: &str, reason: &str) -> Result<(), AdapterError> {
        crate::vault_bridge::vault_revoke(&self.core, grant_id, reason)
    }

    fn vault_list_grants(&self, session_id: Option<&str>) -> Result<Vec<GrantInfo>, AdapterError> {
        crate::vault_bridge::vault_list_grants(&self.core, session_id)
    }

    fn vault_add(&self, p: VaultAddParams) -> Result<VaultAddInfo, AdapterError> {
        crate::vault_bridge::vault_add(&self.core, p)
    }

    // ── v0.9.4 W6 direct credential bridge methods ──────────────────

    fn vault_set_secret(
        &self,
        p: VaultSetSecretParams,
    ) -> Result<VaultSetSecretInfo, AdapterError> {
        crate::vault_bridge::vault_set_secret(&self.core, p)
    }

    fn vault_delete_secret(
        &self,
        p: VaultDeleteSecretParams,
    ) -> Result<VaultDeleteSecretInfo, AdapterError> {
        crate::vault_bridge::vault_delete_secret(&self.core, p)
    }

    fn vault_list_labels(
        &self,
        p: VaultListLabelsParams,
    ) -> Result<VaultListLabelsInfo, AdapterError> {
        crate::vault_bridge::vault_list_labels(&self.core, p)
    }

    fn vault_get_session_context(
        &self,
    ) -> Result<
        loom_rpc::core_service_adapter::core_service_adapter::VaultGetSessionContextInfo,
        AdapterError,
    > {
        crate::vault_bridge::vault_get_session_context(&self.core)
    }

    fn vault_diagnose(&self) -> Result<VaultDiagnoseInfo, AdapterError> {
        crate::vault_bridge::vault_diagnose(&self.core)
    }

    fn gc_run(
        &self,
        ttl_days: Option<u64>,
        _store_max_bytes: Option<u64>,
    ) -> Result<loom_rpc::core_service_adapter::core_service_adapter::GcRunReport, AdapterError>
    {
        // GC the content store. ttl_days defaults to 7
        // when unset; the AC mandates the default is documented and
        // tested. Honor `store_max_bytes` later via auto-GC threshold
        // (orthogonal to manual `loom gc`).
        let ttl_secs = ttl_days.unwrap_or(7).saturating_mul(24 * 3600);
        let ttl = std::time::Duration::from_secs(ttl_secs);
        let report = self
            .core
            .content_store
            .gc(ttl)
            .map_err(|e| map_loom_error(&e))?;
        Ok(
            loom_rpc::core_service_adapter::core_service_adapter::GcRunReport {
                blobs_scanned: report.blobs_scanned,
                blobs_collected: report.blobs_collected,
                bytes_freed: report.bytes_freed,
            },
        )
    }

    fn session_reap(
        &self,
        dry_run: bool,
    ) -> Result<loom_rpc::core_service_adapter::core_service_adapter::ReapReport, AdapterError>
    {
        // Skip set = sessions live in memory, so a session merely mid-WAL-write
        // is never mistaken for an abandoned corrupt orphan (D4).
        //
        // TOCTOU note: a session created AFTER this snapshot but scanned before
        // it finishes writing is not in `skip`. Two independent guards make a
        // false quarantine effectively impossible: (1) a freshly created session
        // writes a valid Header synchronously in `open_manifest` before
        // `create_session` returns, so `is_corrupt_orphan` sees a well-formed
        // (non-corrupt) WAL and leaves it alone; (2) reap is only needed once
        // corrupt phantoms have saturated the cap — and then `session.create`
        // is already rejected with `SessionCapExceeded`, so nothing new can
        // race in. Quarantine is non-destructive (dir moved aside, not deleted), so
        // even a pathological miss is recoverable by moving the dir back.
        let skip = self.core.session_manager.live_session_ids();
        let outcome = self
            .core
            .startup_manager
            .quarantine_corrupt_sessions(dry_run, &skip)
            .map_err(|e| map_loom_error(&e))?;

        if !outcome.quarantined.is_empty() || !outcome.failed.is_empty() {
            tracing::warn!(
                metric = "loom_daemon_session_reap",
                dry_run,
                quarantined = outcome.quarantined.len(),
                skipped_live = outcome.skipped_live,
                failed = outcome.failed.len(),
                "session.reap quarantined corrupt orphan session(s)"
            );
        }

        // Also reap leaked LIVE resources: idle/zombie sessions + orphan Chromium trees.
        // `apply = !dry_run` so a dry-run only previews. The sweep is async; run it on the
        // current multi-thread runtime via block_in_place (we're inside a sync adapter call
        // dispatched from the async RPC loop). Best-effort: a sweep failure must not fail the
        // corrupt-WAL reap that already succeeded.
        let sweep = {
            let cfg = reaper::ReaperConfig::from_env();
            let core = Arc::clone(&self.core);
            let host = self.wasm_host.clone();
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async move {
                    reaper::run_sweep(&core, host.as_ref(), &cfg, !dry_run).await
                })
            })
        };

        Ok(
            loom_rpc::core_service_adapter::core_service_adapter::ReapReport {
                quarantined: outcome.quarantined.into_iter().map(|s| s.0).collect(),
                skipped_live: outcome.skipped_live,
                dry_run: outcome.dry_run,
                quarantine_dir: outcome.quarantine_dir.map(|p| p.display().to_string()),
                failed: outcome
                    .failed
                    .into_iter()
                    .map(|f| format!("{}: {}", f.session_id.0, f.details))
                    .collect(),
                idle_evicted: sweep.idle_evicted,
                zombies_closed: sweep.zombies_closed,
                orphan_browsers_killed: sweep.orphan_browsers_killed,
                orphan_dirs_removed: sweep.orphan_dirs_removed,
            },
        )
    }
}
