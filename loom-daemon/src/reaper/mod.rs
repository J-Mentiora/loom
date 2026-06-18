//! Session + Chromium reaper.
//!
//! ONE sequential sweep (intake-council C1: no concurrent races between the phases) runs
//! three steps in order, used both by the periodic background task and the on-demand
//! `session.reap` RPC (the latter in preview mode when `apply == false`). The C1
//! single-sweep invariant is enforced by construction: every `run_sweep` entry point
//! serializes on `SWEEP_GATE`, so an RPC-triggered sweep can never overlap the periodic
//! ticker (or another RPC):
//!
//! 1. **idle eviction** — Active sessions idle past the TTL with no in-flight action, closed
//!    through the two-phase guarded `evict_if_idle` + browser teardown.
//! 2. **zombie detection** — Active sessions whose Chromium pid is already dead, closed.
//! 3. **orphan GC** — `$TMPDIR/loom-chromium-*` dirs whose session is not live and which are
//!    aged past one sweep interval; their browser tree is `killpg`'d (pidfile-verified) and
//!    the dir removed — but ONLY once the tree is confirmed dead. A kill skipped for a live,
//!    unverifiable pid retains the dir + pidfile so the next sweep can retry.
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

/// Browser liveness for an Active session, via its sidecar pidfile. Probes the process
/// GROUP, not just the leader pid: Chromium's leader can exit while renderer/GPU members
/// survive, and such a session is NOT a zombie (its tree still holds fds/memory). Missing
/// pidfile ⇒ treated as ALIVE (fail-safe: a session that never navigated has no browser and
/// must not be mistaken for a zombie).
fn browser_pid_alive_for(session_id: &str) -> bool {
    match proc::read_pidfile(&session_udd(session_id)) {
        Some(pgid) => proc::group_is_alive(pgid),
        None => true,
    }
}

/// Tear down a session's browser via the host (cooperative shim shutdown + profile cleanup).
async fn teardown_browser(host: Option<&Arc<WasmHost>>, session_id: &str) {
    if let Some(host) = host {
        host.shutdown_session(session_id).await;
    }
}

/// Outcome of a `kill_orphan` attempt, deciding whether the user-data-dir may be swept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OrphanKill {
    /// Tree verified + signalled — it is dead now.
    Terminated,
    /// Nothing alive at the recorded pgid — tree confirmed dead.
    AlreadyDead,
    /// No verifiable pid recorded — there is no process this sweep could strand.
    NoPidfile,
    /// A LIVE pid the sweep declined to signal (cmdline did not verify — recycled pid, or a
    /// transient cmdline-read failure). The dir AND pidfile must survive so the next sweep
    /// can retry: deleting them would make a still-running browser permanently untrackable,
    /// inverting the module's "leak for ONE MORE SWEEP" invariant.
    SkippedAliveUnverified,
}

impl OrphanKill {
    /// Whether the browser tree is confirmed dead (or never recorded a pid), making the
    /// user-data-dir safe to remove. `SkippedAliveUnverified` keeps the dir: the pidfile
    /// inside it is the only handle a future sweep has on the still-running process.
    fn dir_removable(self) -> bool {
        !matches!(self, OrphanKill::SkippedAliveUnverified)
    }
}

/// Kill an orphan browser tree for `udd`. Pidfile-only + cmdline-verified (hardening B/C3):
/// any uncertainty ⇒ no signal sent. The returned tri-state tells the caller whether the
/// dir may be removed (`dir_removable`) — only a confirmed-dead tree is sweepable.
async fn kill_orphan(udd: std::path::PathBuf, grace: Duration) -> OrphanKill {
    let Some(pgid) = proc::read_pidfile(&udd) else {
        // No verifiable pid — fail-safe skip (no signal). With no pid recorded there is
        // nothing a dir removal could strand, so the caller may still sweep the dir.
        return OrphanKill::NoPidfile;
    };
    // Group liveness, not leader liveness: a dead leader with surviving renderer/GPU
    // members is exactly the leaked tree this GC exists for — kill(leader, 0) would
    // report it AlreadyDead and the stragglers would never be signalled.
    if !proc::group_is_alive(pgid) {
        return OrphanKill::AlreadyDead;
    }
    let needle = format!("user-data-dir={}", udd.display());
    // C3: only signal a group whose command line actually references this user-data-dir, so
    // a recycled pid is never hit. Checked across the GROUP members (leader fast-path
    // first) so a dead-leader tree can still be verified and killed.
    let verified = tokio::task::spawn_blocking(move || proc::group_cmdline_contains(pgid, &needle))
        .await
        .unwrap_or(false);
    if !verified {
        tracing::warn!(
            pid = pgid,
            udd = %udd.display(),
            "reaper: orphan pid did not match user-data-dir (recycled?); \
             skipping kill, retaining dir for the next sweep"
        );
        return OrphanKill::SkippedAliveUnverified;
    }
    let grace2 = grace;
    match tokio::task::spawn_blocking(move || proc::terminate_group(pgid, grace2)).await {
        Ok(proc::KillOutcome::Terminated) => OrphanKill::Terminated,
        Ok(proc::KillOutcome::AlreadyDead) => OrphanKill::AlreadyDead,
        // Join error (kill task panicked/cancelled): tree state unknown while the pid was
        // just seen alive — keep the dir so the next sweep retries.
        Err(_) => OrphanKill::SkippedAliveUnverified,
    }
}

/// Serializes every sweep entry point — startup GC, the periodic ticker, and the
/// `session.reap` RPC (including two concurrent RPCs) — so the documented C1
/// single-sequential-sweep invariant holds by construction instead of by scheduling luck.
/// Overlapping sweeps would double-signal orphan trees, race `remove_orphan_dir` against a
/// concurrent kill's pidfile/cmdline verification, and double-count `SweepReport`s. Sweeps
/// are idempotent, so a caller that waited here just re-scans and reports the (now empty)
/// remainder.
static SWEEP_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Run one reaper sweep. `apply == false` previews (no side effects) — used by
/// `session.reap` dry-run. Idempotent: a second apply finds nothing left and reports zero.
/// Sweeps are serialized on `SWEEP_GATE` (see above); a concurrent caller waits, it is
/// never skipped — `session.reap` callers always get a real (possibly empty) report.
pub async fn run_sweep(
    core: &Arc<CoreApiFacade>,
    host: Option<&Arc<WasmHost>>,
    cfg: &ReaperConfig,
    apply: bool,
) -> SweepReport {
    let _sweep_guard = SWEEP_GATE.lock().await;
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
            let outcome = kill_orphan(entry.path.clone(), cfg.kill_grace).await;
            if outcome == OrphanKill::Terminated {
                tracing::warn!(
                    metric = "loom_reaper_orphan_killed",
                    session_id = %sid,
                    udd = %entry.path.display(),
                    "reaper: killed orphan Chromium tree"
                );
                report.orphan_browsers_killed.push(sid.clone());
            }
            // Only sweep a dir whose tree is confirmed dead. When the kill was skipped for a
            // live, unverifiable pid the udd + pidfile must outlive this sweep — removing
            // them would permanently strand the process (no future sweep, `session.reap`,
            // or `loom doctor` could ever find it again).
            if !outcome.dir_removable() {
                continue;
            }
            if proc::remove_orphan_dir(&sid, &entry.path) {
                report.orphan_dirs_removed += 1;
            }
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    fn scratch_udd(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("loom-chromium-{name}{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch udd");
        dir
    }

    fn make_core(data_root: &std::path::Path) -> Arc<CoreApiFacade> {
        let config = loom_core::core_api_facade::CoreConfig {
            data_root: data_root.to_path_buf(),
            log_path: data_root.join("daemon.log"),
            otel_enabled: false,
            default_seed: 42,
            checkpoint_every_n: 100,
        };
        let keychain: Arc<dyn loom_core::vault::KeychainAccess> =
            Arc::new(loom_keychain::StubKeychain);
        CoreApiFacade::new(config, keychain).expect("CoreApiFacade::new in scratch dir")
    }

    /// The C1 single-sweep invariant: a sweep entering while another holds the gate must
    /// WAIT (not overlap, not be skipped) and complete once the gate frees.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_sweep_waits_for_an_in_flight_sweep() {
        let tmp = std::env::temp_dir().join(format!("reaper-gate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let core = make_core(&tmp);
        let cfg = ReaperConfig {
            idle_ttl: Duration::ZERO,
            sweep_interval: Duration::from_secs(60),
            kill_grace: Duration::from_millis(100),
            orphan_min_age: Duration::from_secs(60),
            orphan_gc_enabled: false,
        };

        // Simulate an in-flight sweep by holding the gate directly.
        let guard = SWEEP_GATE.lock().await;
        let core2 = Arc::clone(&core);
        let cfg2 = cfg.clone();
        let task = tokio::spawn(async move { run_sweep(&core2, None, &cfg2, true).await });

        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            !task.is_finished(),
            "run_sweep must block while another sweep holds the gate (C1: no overlap)"
        );

        drop(guard);
        let report = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("sweep must complete once the gate frees")
            .expect("sweep task must not panic");
        assert!(report.is_empty(), "empty facade ⇒ empty report");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn orphan_kill_dir_removable_only_when_tree_confirmed_dead() {
        assert!(OrphanKill::Terminated.dir_removable());
        assert!(OrphanKill::AlreadyDead.dir_removable());
        assert!(OrphanKill::NoPidfile.dir_removable());
        // The one case that must retain the udd + pidfile for retry.
        assert!(!OrphanKill::SkippedAliveUnverified.dir_removable());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn kill_orphan_no_pidfile_is_removable_no_signal() {
        let udd = scratch_udd("no-pidfile-");
        let outcome = kill_orphan(udd.clone(), Duration::from_millis(100)).await;
        assert_eq!(outcome, OrphanKill::NoPidfile);
        assert!(outcome.dir_removable());
        let _ = std::fs::remove_dir_all(&udd);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn kill_orphan_dead_pid_is_already_dead() {
        let udd = scratch_udd("dead-pid-");
        // A very high pid is almost certainly unused (same fixture as proc::tests).
        std::fs::write(udd.join(proc::PIDFILE_NAME), "2000000000").unwrap();
        let outcome = kill_orphan(udd.clone(), Duration::from_millis(100)).await;
        assert_eq!(outcome, OrphanKill::AlreadyDead);
        assert!(outcome.dir_removable());
        let _ = std::fs::remove_dir_all(&udd);
    }

    /// The finding's exact scenario: a LIVE pid whose cmdline does NOT reference the udd
    /// (recycled pid / transient read failure). The kill must be skipped AND the dir must
    /// be retained — previously run_sweep removed the dir (and pidfile) anyway, stranding
    /// the process forever.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn kill_orphan_live_unverified_pid_skips_kill_and_retains_dir() {
        let udd = scratch_udd("live-unverified-");
        let mut child = Command::new("sleep")
            .arg("30")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("spawn sleep");
        let pid = child.id() as i32;
        std::fs::write(udd.join(proc::PIDFILE_NAME), pid.to_string()).unwrap();

        let outcome = kill_orphan(udd.clone(), Duration::from_millis(100)).await;
        assert_eq!(outcome, OrphanKill::SkippedAliveUnverified);
        assert!(
            !outcome.dir_removable(),
            "udd + pidfile must survive for the next sweep"
        );
        // The unverified process must NOT have been signalled.
        assert!(proc::pid_is_alive(pid), "skip means no signal was sent");

        let _ = proc::terminate_group(pid, Duration::from_millis(500));
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&udd);
    }

    /// Happy path: live pid whose cmdline carries the user-data-dir needle → verified,
    /// terminated, dir removable.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn kill_orphan_verified_live_tree_is_terminated() {
        let udd = scratch_udd("live-verified-");
        let needle = format!("user-data-dir={}", udd.display());
        // Extra argv after `-c CMD` becomes $0/$1 of the shell — visible in its command
        // line, so the cmdline verification matches without launching a real chromium.
        // The body MUST be a compound command (`; :`): a *single* simple command lets the
        // shell exec-optimize itself into `sleep`, which drops $0/$1 (and the needle) from
        // the argv. With one command the test only passed by winning a race against that
        // exec — flaky under parallel scheduling. The compound form keeps the shell process
        // alive carrying the needle, so verification is timing-independent.
        let mut child = Command::new("sh")
            .args(["-c", "sleep 30; :", "sh", &needle])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("spawn sh sleep");
        let pid = child.id() as i32;
        std::fs::write(udd.join(proc::PIDFILE_NAME), pid.to_string()).unwrap();

        // Wait until the shell's argv (carrying the needle) is observable to the verifier.
        // Under parallel execution the child can still be starting up when kill_orphan reads
        // the cmdline; polling (bounded) instead of assuming instant readiness removes the
        // last spawn-timing race.
        let mut ready = false;
        for _ in 0..200 {
            if proc::group_cmdline_contains(pid, &needle) {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            ready,
            "child cmdline never became observable within 2s — environment too contended to test the reaper"
        );

        let outcome = kill_orphan(udd.clone(), Duration::from_millis(1000)).await;
        assert_eq!(outcome, OrphanKill::Terminated);
        assert!(outcome.dir_removable());

        let _ = child.wait();
        assert!(!proc::pid_is_alive(pid));
        let _ = std::fs::remove_dir_all(&udd);
    }
}
