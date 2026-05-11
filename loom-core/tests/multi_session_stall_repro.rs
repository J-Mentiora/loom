// Multi-session stall repro — exercises the SessionScope-driven cleanup
// path that replaced the fire-and-forget tokio::spawn leak.
//
// Background: prior to this fix, the daemon stalled after 4-6 sequential
// client sessions because (a) `LocalSessionManager::close` aborted the
// budget timer JoinHandle without joining it; (b) shim teardown was
// fire-and-forget; (c) the connection task had no per-request deadline.
// After 4-6 cycles, accumulated never-joined tasks saturated the tokio
// runtime and `session.create` blocked forever.
//
// These tests don't drive a real shim subprocess (that's a follow-up
// using `loom-host`'s integration test pattern with `fake-chromium`);
// they verify the LIBRARY-level fix at the SessionManager + SessionScope
// boundary. The smoke test covers the user-facing success criterion:
// "10+ sequential sessions complete without daemon stall."

use loom_core::budget_enforcer::{BudgetEnforcer, BudgetLimits, LocalBudgetEnforcer};
use loom_core::content_store::{ContentStore, LocalContentStore};
use loom_core::determinism_harness::DeterminismHarness;
use loom_core::error::LoomError;
use loom_core::manifest_writer::{LocalManifestWriter, ManifestWriter};
use loom_core::observability::Observability;
use loom_core::session_manager::{LocalSessionManager, SessionCreateOpts, SessionStatus};
use loom_core::session_scope::{DrainOutcome, SessionScope};
use loom_core::vault::{KeychainAccess, LocalVault, Vault};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

struct StubKc;
impl KeychainAccess for StubKc {
    fn get_secret(&self, _label: &str) -> Result<Zeroizing<Vec<u8>>, LoomError> {
        Ok(Zeroizing::new(vec![0u8; 16]))
    }
}

struct Env {
    sm: Arc<LocalSessionManager>,
    _tmp: tempfile::TempDir,
}

fn make_env() -> Env {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let obs = Observability::new(root.join("loom.log"), false);
    let cs: Arc<dyn ContentStore> =
        Arc::new(LocalContentStore::new(root.join("store"), obs.clone()));
    let mw: Arc<dyn ManifestWriter> =
        Arc::new(LocalManifestWriter::new(root.join("sessions"), obs.clone()));
    let kc: Arc<dyn KeychainAccess> = Arc::new(StubKc);
    let v: Arc<dyn Vault> = Arc::new(LocalVault::new(kc, mw.clone(), obs.clone()));
    let be: Arc<dyn BudgetEnforcer> = Arc::new(LocalBudgetEnforcer::new(obs.clone()));
    let dh = Arc::new(DeterminismHarness::new(42, mw.clone()));
    let sm = LocalSessionManager::new(cs, mw, v, be, dh, obs, 0, root.join("sessions"));
    Env { sm, _tmp: tmp }
}

fn opts(limits: Option<BudgetLimits>) -> SessionCreateOpts {
    SessionCreateOpts {
        agent_id: "stress-test".into(),
        surface: "web".into(),
        seed: Some(0),
        limits,
        replay_of: None,
        started_at_ms_override: None,
        capture_policy: None,
        no_blocklist: false,
        profile: "safe".to_string(),
    }
}

/// Success criterion from the investigation prompt: 10 sequential
/// sessions must each complete create+close in well under the previous
/// stall window (≤ 1 second per cycle is generous for a no-shim path).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ten_sequential_sessions_create_and_close_within_one_second_each() {
    let env = make_env();

    for i in 0..10 {
        let cycle_start = Instant::now();
        let id = env
            .sm
            .create(opts(Some(BudgetLimits {
                session_walltime_ms: 60_000,
                ..BudgetLimits::default()
            })))
            .expect("create");
        env.sm.close(id.clone()).expect("close");
        let elapsed = cycle_start.elapsed();
        assert!(
            elapsed < Duration::from_secs(1),
            "session #{i} create+close took {elapsed:?} — far exceeds 1s budget"
        );
    }
}

/// Verifies the structural fix: budget timers spawned per-session are
/// owned by the session's SessionScope and torn down by close, NOT
/// leaked into the runtime. With the pre-fix code, 100 sessions with
/// budget timers would leak 100 JoinHandles (and the timer fires would
/// have to be drained by the kill callback's idempotency guard). After
/// the fix, scope.cancel() inside close() signals every timer to exit
/// cooperatively before its sleep completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_hundred_sequential_sessions_with_budget_timer_complete() {
    let env = make_env();

    let started = Instant::now();
    for _ in 0..100 {
        let id = env
            .sm
            .create(opts(Some(BudgetLimits {
                session_walltime_ms: 60_000,
                ..BudgetLimits::default()
            })))
            .expect("create");
        env.sm.close(id).expect("close");
    }
    let total = started.elapsed();
    assert!(
        total < Duration::from_secs(10),
        "100 sessions took {total:?} — likely a regression"
    );
}

/// SessionScope-level smoke: 50 scopes spawned in parallel, each with
/// 5 cooperative children. All drain cleanly; no `AbortedSurvivors`.
/// Catches regressions in the cooperative-then-abort drain logic.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_scopes_with_cooperative_children_all_drain_clean() {
    let counter = Arc::new(AtomicUsize::new(0));
    let mut joins = Vec::new();
    for _ in 0..50 {
        let counter = Arc::clone(&counter);
        joins.push(tokio::spawn(async move {
            let scope = SessionScope::new();
            for _ in 0..5 {
                let token = scope.child_token();
                let counter = Arc::clone(&counter);
                scope.spawn(async move {
                    tokio::select! {
                        _ = token.cancelled() => {
                            counter.fetch_add(1, Ordering::SeqCst);
                        }
                        _ = tokio::time::sleep(Duration::from_secs(60)) => {}
                    }
                });
            }
            scope.drain(Duration::from_secs(2)).await
        }));
    }
    for j in joins {
        let outcome = j.await.expect("scope task panicked");
        assert_eq!(outcome, DrainOutcome::Clean, "scope drain failed cleanly");
    }
    assert_eq!(
        counter.load(Ordering::SeqCst),
        250,
        "expected all 50*5 children to observe cancellation"
    );
}

/// Closed sessions reach SessionStatus::Closed and are queryable. The
/// pre-fix bug would have left them in `Active` (or partially closed)
/// when budget-timer abort interleaved badly with the close. This is a
/// regression guard for that interaction.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_close_status_is_closed_and_kill_reason_remains_none() {
    let env = make_env();
    let id = env
        .sm
        .create(opts(Some(BudgetLimits {
            session_walltime_ms: 100,
            ..BudgetLimits::default()
        })))
        .expect("create");
    let session = env.sm.get(id.clone()).expect("get");
    env.sm.close(id).expect("close");
    assert_eq!(*session.status.lock(), SessionStatus::Closed);

    // Wait past the budget — kill_reason MUST remain None because the
    // scope-owned timer should have observed cancellation.
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        session.kill_reason.lock().is_none(),
        "closed session must not be kill-tagged"
    );
}
