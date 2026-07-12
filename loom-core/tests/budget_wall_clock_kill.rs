// Tests for the budget-wall-clock-not-enforced feature.
//
// Coverage:
//   - --budget wall_clock=1s kills session at expiry; subsequent
//     dispatch rejects.
//   - kill receipt carries budget_kind + elapsed_ms via
//     `Session::kill_reason` field populated by the SessionManager
//     kill callback.
//   - --budget network=NMB enforces and kills similarly
//     (re-uses the same kill path).
//   - kill happens within 200ms of budget expiry.

use loom_core::budget_enforcer::{
    BudgetEnforcer, BudgetLimits, KillReason, LocalBudgetEnforcer, ResourceKind,
};
use loom_core::content_store::{ContentStore, LocalContentStore};
use loom_core::error::LoomErrorCode;
use loom_core::manifest_writer::{LocalManifestWriter, ManifestWriter, SessionId};
use loom_core::observability::Observability;
use loom_core::session_manager::{
    AbortReason, LocalSessionManager, SessionCreateOpts, SessionStatus,
};
use loom_core::vault::{KeychainAccess, LocalVault, Vault};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

struct StubKc;
impl KeychainAccess for StubKc {
    fn get_secret(&self, _label: &str) -> Result<Zeroizing<Vec<u8>>, loom_keychain::KeychainError> {
        Ok(Zeroizing::new(vec![0u8; 16]))
    }
    fn set_secret(
        &self,
        _label: &str,
        _secret: Zeroizing<Vec<u8>>,
    ) -> Result<(), loom_keychain::KeychainError> {
        Err(loom_keychain::KeychainError::new(
            loom_keychain::KeychainErrorKind::Unavailable,
            "test stub",
        ))
    }
    fn delete_secret(&self, _label: &str) -> Result<(), loom_keychain::KeychainError> {
        Err(loom_keychain::KeychainError::new(
            loom_keychain::KeychainErrorKind::Unavailable,
            "test stub",
        ))
    }
    fn list_labels(&self) -> Result<Vec<String>, loom_keychain::KeychainError> {
        Err(loom_keychain::KeychainError::new(
            loom_keychain::KeychainErrorKind::Unavailable,
            "test stub",
        ))
    }
}

struct Env {
    sm: Arc<LocalSessionManager>,
    be: Arc<dyn BudgetEnforcer>,
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
    let be_concrete = Arc::new(LocalBudgetEnforcer::new(obs.clone()));
    let be: Arc<dyn BudgetEnforcer> = be_concrete.clone();
    let sm = LocalSessionManager::new(cs, mw, v, be.clone(), obs, 0, root.join("sessions"));
    Env { sm, be, _tmp: tmp }
}

fn opts_with_limits(limits: BudgetLimits) -> SessionCreateOpts {
    SessionCreateOpts {
        agent_id: "test-agent".into(),
        surface: "web".into(),
        seed: Some(42),
        limits: Some(limits),
        replay_of: None,
        started_at_ms_override: None,
        capture_policy: None,
        no_blocklist: false,
        no_determinism: false,
        record_screencast: false,
        audio: false,
        profile: "safe".to_string(),
    }
}

// ─── wall-clock kill ─────────────────────────────────────────────────────────

/// Wall-clock budget expiry kills the session (Active → Killed) and trips
/// the abort_flag so any in-flight action's `tokio::select!` race wakes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac_budgetkill_01_wall_clock_expiry_kills_session() {
    let env = make_env();
    let limits = BudgetLimits {
        session_walltime_ms: 100,
        ..BudgetLimits::default()
    };
    let id = env.sm.create(opts_with_limits(limits)).expect("create");
    let session = env.sm.get(id.clone()).expect("get");

    // Wait past the budget. Add slack for tokio scheduling.
    tokio::time::sleep(Duration::from_millis(250)).await;

    assert!(
        session.abort_flag.load(Ordering::Acquire),
        "abort_flag must be set after wall_clock budget expiry"
    );
    assert_eq!(
        *session.status.lock(),
        SessionStatus::Killed,
        "status must transition to Killed"
    );
}

/// After a budget kill, a subsequent BudgetEnforcer::check returns
/// BudgetExceeded — proving the dispatch-time guard fast-fails new actions.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac_budgetkill_01_subsequent_action_rejects_with_budget_exceeded() {
    let env = make_env();
    let limits = BudgetLimits {
        session_walltime_ms: 100,
        ..BudgetLimits::default()
    };
    let id = env.sm.create(opts_with_limits(limits)).expect("create");

    tokio::time::sleep(Duration::from_millis(250)).await;

    let action = loom_core::budget_enforcer::Action {
        action_id: 7,
        kind: "click".into(),
        estimated_walltime_ms: 1,
        estimated_net_bytes: 0,
    };
    let err = env
        .be
        .check(id.clone(), &action)
        .expect_err("check must reject post-kill");
    assert_eq!(err.code, LoomErrorCode::BudgetExceeded);
}

// ─── kill-reason metadata ───────────────────────────────────────────────────

/// The kill callback writes a typed `KillReason::BudgetExceeded` into
/// `Session::kill_reason` BEFORE flipping abort_flag — the executor reads
/// this on the abort arm to stamp `detail.budget_kind` + `detail.elapsed_ms`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac_budgetkill_02_kill_reason_field_populated_with_walltime() {
    let env = make_env();
    let limits = BudgetLimits {
        session_walltime_ms: 100,
        ..BudgetLimits::default()
    };
    let id = env.sm.create(opts_with_limits(limits)).expect("create");
    let session = env.sm.get(id.clone()).expect("get");

    tokio::time::sleep(Duration::from_millis(250)).await;

    let reason = session.kill_reason.lock().clone();
    match reason {
        Some(KillReason::BudgetExceeded {
            kind,
            observed,
            limit,
        }) => {
            assert_eq!(kind, ResourceKind::Walltime, "kind must be Walltime");
            assert_eq!(limit, 100, "limit echoes the configured budget");
            assert!(observed >= 100, "observed must be ≥ limit (got {observed})");
        }
        other => panic!("expected BudgetExceeded(Walltime, ...), got {other:?}"),
    }
}

/// User-initiated abort must NOT set kill_reason — that path remains
/// distinguishable as a plain ActionOutcome::Aborted (not Trapped).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac_budgetkill_02_user_abort_leaves_kill_reason_none() {
    let env = make_env();
    let limits = BudgetLimits {
        session_walltime_ms: 60_000,
        ..BudgetLimits::default()
    };
    let id = env.sm.create(opts_with_limits(limits)).expect("create");
    let session = env.sm.get(id.clone()).expect("get");

    env.sm
        .abort(
            id.clone(),
            AbortReason {
                reason: "user_request".into(),
            },
        )
        .expect("abort");

    assert!(
        session.kill_reason.lock().is_none(),
        "kill_reason must remain None for user-initiated abort"
    );
    assert_eq!(*session.status.lock(), SessionStatus::Aborted);
}

// ─── network-bytes kill ─────────────────────────────────────────────────────

/// Network budget kill flows through the same plumbing as wall-clock:
/// `account(Network, ...)` past threshold fires the kill callback, which
/// writes kill_reason and flips abort_flag.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac_budgetkill_03_network_budget_uses_same_kill_path() {
    let env = make_env();
    let limits = BudgetLimits {
        session_walltime_ms: 600_000, // far in the future — wall clock won't fire
        network_bytes: 1024,
        ..BudgetLimits::default()
    };
    let id = env.sm.create(opts_with_limits(limits)).expect("create");
    let session = env.sm.get(id.clone()).expect("get");

    // Drive network accounting past the limit — must fire the kill callback.
    let _ = env.be.account(id.clone(), ResourceKind::Network, 2048);

    let reason = session.kill_reason.lock().clone();
    match reason {
        Some(KillReason::BudgetExceeded { kind, .. }) => {
            assert_eq!(kind, ResourceKind::Network, "kind must be Network");
        }
        other => panic!("expected BudgetExceeded(Network, ...), got {other:?}"),
    }
    assert!(session.abort_flag.load(Ordering::Acquire));
    assert_eq!(*session.status.lock(), SessionStatus::Killed);
}

// ─── kill latency ───────────────────────────────────────────────────────────

/// Kill must happen within 200ms of budget expiry (wall-clock tolerance).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ac_budgetkill_04_kill_within_200ms_of_expiry() {
    let env = make_env();
    let budget_ms = 100u64;
    let limits = BudgetLimits {
        session_walltime_ms: budget_ms,
        ..BudgetLimits::default()
    };
    let create_t = Instant::now();
    let id = env.sm.create(opts_with_limits(limits)).expect("create");
    let session = env.sm.get(id.clone()).expect("get");

    // Poll abort_flag every 5ms up to 500ms.
    let kill_t = loop {
        if session.abort_flag.load(Ordering::Acquire) {
            break Instant::now();
        }
        if create_t.elapsed() > Duration::from_millis(500) {
            panic!("abort_flag never set within 500ms of session create");
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };

    let observed_kill_ms = kill_t.duration_since(create_t).as_millis() as u64;
    let latency_past_budget = observed_kill_ms.saturating_sub(budget_ms);
    assert!(
        latency_past_budget <= 200,
        "kill must fire within 200ms of budget expiry; observed {latency_past_budget}ms past expiry (total {observed_kill_ms}ms)"
    );
}

// ─── Lifecycle / cleanup ─────────────────────────────────────────────────────

/// close() must cancel the budget timer task. Otherwise a closed session
/// would still trip the kill callback at expiry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_cancels_budget_timer() {
    let env = make_env();
    let limits = BudgetLimits {
        session_walltime_ms: 100,
        ..BudgetLimits::default()
    };
    let id = env.sm.create(opts_with_limits(limits)).expect("create");
    let session = env.sm.get(id.clone()).expect("get");

    // Close BEFORE the budget would fire.
    env.sm.close(id.clone()).expect("close");
    assert_eq!(*session.status.lock(), SessionStatus::Closed);

    // Wait past the budget — kill_reason must remain None and status Closed.
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        session.kill_reason.lock().is_none(),
        "closed session must not be kill-tagged"
    );
    assert_eq!(*session.status.lock(), SessionStatus::Closed);
}

/// Replay sessions skip budget enforcement — the manifest header pins the
/// original budget; re-running a wall-clock timer would corrupt determinism.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replay_sessions_skip_budget_timer() {
    let env = make_env();
    let limits = BudgetLimits {
        session_walltime_ms: 100,
        ..BudgetLimits::default()
    };
    let opts = SessionCreateOpts {
        agent_id: "test-agent".into(),
        surface: "web".into(),
        seed: Some(42),
        limits: Some(limits),
        replay_of: Some(SessionId("01HZSOURCE".into())),
        started_at_ms_override: Some(0),
        capture_policy: None,
        no_blocklist: false,
        no_determinism: false,
        record_screencast: false,
        audio: false,
        profile: "safe".to_string(),
    };
    let id = env.sm.create(opts).expect("create replay");
    let session = env.sm.get(id.clone()).expect("get");

    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        !session.abort_flag.load(Ordering::Acquire),
        "replay session must not be killed by wall-clock timer"
    );
    assert!(session.kill_reason.lock().is_none());
}

// ─── Budget JSON roundtrip ───────────────────────────────────────────────────

/// `BudgetLimits` survives a serde_json roundtrip — the daemon's
/// `CoreBridge::create_session_raw` deserialises `params.budget` into a
/// `BudgetLimits` and threads it through `SessionCreateOpts.limits`.
#[test]
fn budget_limits_json_roundtrips_through_serde_value() {
    let limits = BudgetLimits {
        session_walltime_ms: 1000,
        action_walltime_ms: 5000,
        network_bytes: 10 * 1024 * 1024,
        dom_nodes: 12_000,
        js_heap_bytes: 256 * 1024 * 1024,
    };
    let value = serde_json::to_value(limits).expect("to_value");
    let restored: BudgetLimits = serde_json::from_value(value).expect("from_value");
    assert_eq!(restored.session_walltime_ms, 1000);
    assert_eq!(restored.action_walltime_ms, 5000);
    assert_eq!(restored.network_bytes, 10 * 1024 * 1024);
    assert_eq!(restored.dom_nodes, 12_000);
    assert_eq!(restored.js_heap_bytes, 256 * 1024 * 1024);
}
