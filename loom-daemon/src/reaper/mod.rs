//! Session + Chromium reaper.
//!
//! ONE sequential sweep (intake-council C1: no concurrent races between the phases) runs
//! three steps in order, used both by the periodic background task and the on-demand
//! `session.reap` RPC (the latter in preview mode when `apply == false`):
//!
//! 1. **idle eviction** — Active sessions idle past the TTL with no in-flight action, closed
//!    through the two-phase guarded `evict_if_idle` + browser teardown.
//! 2. **zombie detection** — Active sessions whose Chromium pid is already dead, closed.
//! 3. **orphan GC** — `$TMPDIR/loom-chromium-*` dirs whose session is not live and which are
//!    aged past one sweep interval; their browser tree is `killpg`'d (pidfile-verified) and
//!    the dir removed.
//!
//! The decision logic is the pure `decide` module; the I/O is the `proc` module. Every
//! eviction / kill emits a structured `tracing` line so the wedge is visible in logs.

pub mod decide;
pub mod proc;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use loom_core::core_api_facade::core_api_facade::CoreApiFacade;
use loom_core::manifest_writer::SessionId;
use loom_host::WasmHost;

use decide::{SessionView, ZombieView};

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

/// Reaper tunables, read once from the environment at daemon start.
#[derive(Debug, Clone)]
pub struct ReaperConfig {
    /// Idle TTL; `Duration::ZERO` disables idle eviction (`LOOM_SESSION_IDLE_TTL_SECS=0`).
    pub idle_ttl: Duration,
    /// Period between background sweeps (`LOOM_REAPER_SWEEP_SECS`, default 60). Also the
    /// orphan grace window (a dir must be older than this before GC).
    pub sweep_interval: Duration,
    /// SIGTERM→SIGKILL grace per orphan tree (`LOOM_REAP_KILL_GRACE_MS`, default 2000).
    pub kill_grace: Duration,
    /// Minimum age a `loom-chromium-*` dir must reach before orphan GC will touch it
    /// (`LOOM_REAP_ORPHAN_MIN_AGE_SECS`, default 60). Defends the create-race (a just-created
    /// session's dir is spared until older than this). Decoupled from the sweep cadence so
    /// operators can tune the grace independently.
    pub orphan_min_age: Duration,
    /// Whether orphan-browser GC runs (`LOOM_REAPER_ORPHAN_GC`, default on). On even when
    /// idle-TTL is disabled — leaked browsers from prior daemons must still be reaped.
    pub orphan_gc_enabled: bool,
}

impl ReaperConfig {
    pub fn from_env() -> Self {
        let orphan_gc_enabled = !matches!(
            std::env::var("LOOM_REAPER_ORPHAN_GC").ok().as_deref(),
            Some("0") | Some("false") | Some("no")
        );
        ReaperConfig {
            idle_ttl: Duration::from_secs(env_u64("LOOM_SESSION_IDLE_TTL_SECS", 1800)),
            sweep_interval: Duration::from_secs(env_u64("LOOM_REAPER_SWEEP_SECS", 60).max(1)),
            kill_grace: Duration::from_millis(env_u64("LOOM_REAP_KILL_GRACE_MS", 2000)),
            orphan_min_age: Duration::from_secs(env_u64("LOOM_REAP_ORPHAN_MIN_AGE_SECS", 60)),
            orphan_gc_enabled,
        }
    }

    fn idle_ttl_ms(&self) -> u64 {
        self.idle_ttl.as_millis() as u64
    }

    /// Whether the periodic sweep has any work to do at all.
    pub fn periodic_enabled(&self) -> bool {
        self.idle_ttl_ms() > 0 || self.orphan_gc_enabled
    }
}

/// What a sweep did (or, in preview mode, would do).
#[derive(Debug, Clone, Default)]
pub struct SweepReport {
    pub idle_evicted: Vec<String>,
    pub zombies_closed: Vec<String>,
    pub orphan_browsers_killed: Vec<String>,
    pub orphan_dirs_removed: u64,
}

impl SweepReport {
    pub fn is_empty(&self) -> bool {
        self.idle_evicted.is_empty()
            && self.zombies_closed.is_empty()
            && self.orphan_browsers_killed.is_empty()
            && self.orphan_dirs_removed == 0
    }
}

/// The user-data-dir for a session id (mirrors the host's
/// `std::env::temp_dir().join("loom-chromium-<id>")`).
fn session_udd(session_id: &str) -> std::path::PathBuf {
    proc::browser_tmp_root().join(format!("loom-chromium-{session_id}"))
}

/// Browser liveness for an Active session, via its sidecar pidfile. Missing pidfile ⇒
/// treated as ALIVE (fail-safe: a session that never navigated has no browser and must not
/// be mistaken for a zombie).
fn browser_pid_alive_for(session_id: &str) -> bool {
    match proc::read_pidfile(&session_udd(session_id)) {
        Some(pid) => proc::pid_is_alive(pid),
        None => true,
    }
}

/// Tear down a session's browser via the host (cooperative shim shutdown + profile cleanup).
async fn teardown_browser(host: Option<&Arc<WasmHost>>, session_id: &str) {
    if let Some(host) = host {
        host.shutdown_session(session_id).await;
    }
}

/// Kill an orphan browser tree for `udd`. Pidfile-only + cmdline-verified (hardening B/C3):
/// any uncertainty ⇒ no signal sent. Returns true iff a tree was actually terminated.
async fn kill_orphan(udd: std::path::PathBuf, grace: Duration) -> bool {
    let Some(pgid) = proc::read_pidfile(&udd) else {
        // No verifiable pid — fail-safe skip. The dir is still removed by the caller.
        return false;
    };
    if !proc::pid_is_alive(pgid) {
        return false;
    }
    let needle = format!("user-data-dir={}", udd.display());
    // C3: only signal a pid whose command line actually references this user-data-dir, so a
    // recycled pid is never hit.
    let verified = tokio::task::spawn_blocking(move || proc::pid_cmdline_contains(pgid, &needle))
        .await
        .unwrap_or(false);
    if !verified {
        tracing::warn!(
            pid = pgid,
            udd = %udd.display(),
            "reaper: orphan pid did not match user-data-dir (recycled?); skipping kill"
        );
        return false;
    }
    let grace2 = grace;
    matches!(
        tokio::task::spawn_blocking(move || proc::terminate_group(pgid, grace2))
            .await
            .unwrap_or(proc::KillOutcome::AlreadyDead),
        proc::KillOutcome::Terminated
    )
}

/// Run one reaper sweep. `apply == false` previews (no side effects) — used by
/// `session.reap` dry-run. Idempotent: a second apply finds nothing left and reports zero.
pub async fn run_sweep(
    core: &Arc<CoreApiFacade>,
    host: Option<&Arc<WasmHost>>,
    cfg: &ReaperConfig,
    apply: bool,
) -> SweepReport {
    let now = now_ms();
    let sm = &core.session_manager;
    let mut report = SweepReport::default();

    // --- 1. idle eviction -----------------------------------------------------------------
    if cfg.idle_ttl_ms() > 0 {
        let snapshot = sm.session_activity_snapshot();
        let views: Vec<SessionView> = snapshot
            .iter()
            .map(|a| SessionView {
                id: a.id.0.clone(),
                last_activity_ms: a.last_activity_ms,
                in_flight: a.in_flight,
                is_active: a.is_active,
            })
            .collect();
        for id in decide::idle_victims(&views, cfg.idle_ttl_ms(), now) {
            if !apply {
                report.idle_evicted.push(id);
                continue;
            }
            // Two-phase: evict_if_idle re-checks idle + in-flight under the status lock.
            match sm.evict_if_idle(SessionId(id.clone()), cfg.idle_ttl_ms(), now) {
                Ok(true) => {
                    teardown_browser(host, &id).await;
                    tracing::info!(
                        metric = "loom_reaper_idle_evicted",
                        session_id = %id,
                        idle_secs = now.saturating_sub(
                            views.iter().find(|v| v.id == id).map_or(now, |v| v.last_activity_ms)
                        ) / 1000,
                        "reaper: evicted idle session"
                    );
                    report.idle_evicted.push(id);
                }
                Ok(false) => {} // became busy / no longer idle under the lock — spared
                Err(e) => tracing::warn!(session_id = %id, error = %e, "reaper: idle evict failed"),
            }
        }
    }

    // --- 2. zombie detection (Active session, dead browser) -------------------------------
    {
        let snapshot = sm.session_activity_snapshot();
        let zviews: Vec<ZombieView> = snapshot
            .iter()
            .filter(|a| a.is_active && !report.idle_evicted.contains(&a.id.0))
            .map(|a| ZombieView {
                id: a.id.0.clone(),
                is_active: true,
                browser_pid_alive: browser_pid_alive_for(&a.id.0),
            })
            .collect();
        for id in decide::zombie_victims(&zviews) {
            if !apply {
                report.zombies_closed.push(id);
                continue;
            }
            match sm.close_with_reason(SessionId(id.clone()), "zombie") {
                Ok(()) => {
                    teardown_browser(host, &id).await;
                    tracing::warn!(
                        metric = "loom_reaper_zombie_closed",
                        session_id = %id,
                        "reaper: closed zombie session (browser pid dead)"
                    );
                    report.zombies_closed.push(id);
                }
                Err(e) => {
                    tracing::warn!(session_id = %id, error = %e, "reaper: zombie close failed")
                }
            }
        }
    }

    // --- 3. orphan-browser GC -------------------------------------------------------------
    if cfg.orphan_gc_enabled {
        let entries = proc::scan_browser_dirs(&proc::browser_tmp_root());
        // PRIMARY guard (C2): a FRESH live-set snapshot taken right before classification.
        let live: HashSet<String> = sm.live_session_ids().iter().map(|s| s.0.clone()).collect();
        let min_age_ms = cfg.orphan_min_age.as_millis() as u64;
        for sid in decide::classify_orphans(&entries, &live, now, min_age_ms) {
            // Re-check the live set immediately before acting (defense-in-depth).
            if live.contains(&sid) {
                continue;
            }
            let Some(entry) = entries.iter().find(|e| e.session_id == sid) else {
                continue;
            };
            if !apply {
                report.orphan_browsers_killed.push(sid);
                continue;
            }
            if kill_orphan(entry.path.clone(), cfg.kill_grace).await {
                tracing::warn!(
                    metric = "loom_reaper_orphan_killed",
                    session_id = %sid,
                    udd = %entry.path.display(),
                    "reaper: killed orphan Chromium tree"
                );
                report.orphan_browsers_killed.push(sid.clone());
            }
            proc::remove_orphan_dir(&sid, &entry.path);
            report.orphan_dirs_removed += 1;
        }
    }

    report
}
