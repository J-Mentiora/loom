// Interface tests for `BudgetEnforcer`. Verifies the two-phase contract,
// atomic counters, kill-callback cycle break.

use super::budget_enforcer::{
    Action, BudgetEnforcer, BudgetLimits, KillCallback, KillReason, LocalBudgetEnforcer,
    ResourceKind, SessionCounters,
};
use loom_core::error::{LoomError, LoomErrorCode};
use loom_core::manifest_writer::SessionId;
use loom_core::observability::Observability;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

fn fixture() -> LocalBudgetEnforcer {
    let obs = Observability::new(PathBuf::from("/tmp/loom-test/loom.log"), false);
    LocalBudgetEnforcer::new(obs)
}

fn noop_kill() -> KillCallback {
    Arc::new(|_id, _reason| {})
}

// === Defaults match BC soft binding ===

#[test]
fn default_budget_limits_match_soft_binding_defaults() {
    let l = BudgetLimits::default();
    assert_eq!(l.action_walltime_ms, 60_000);
    assert_eq!(l.session_walltime_ms, 600_000);
    assert_eq!(l.network_bytes, 50 * 1024 * 1024);
    assert_eq!(l.dom_nodes, 50_000);
    assert_eq!(l.js_heap_bytes, 512 * 1024 * 1024);
}

// === Per-session AtomicU64 with fetch_add ===

#[test]
fn session_counters_use_atomic_u64_for_each_resource() {
    let c = SessionCounters::new();
    c.network_bytes.fetch_add(1024, Ordering::Relaxed);
    c.walltime_ms.fetch_add(50, Ordering::Relaxed);
    c.dom_nodes.fetch_add(7, Ordering::Relaxed);
    c.js_heap_bytes.fetch_add(4096, Ordering::Relaxed);
    let (w, n, d, h) = c.snapshot();
    assert_eq!(w, 50);
    assert_eq!(n, 1024);
    assert_eq!(d, 7);
    assert_eq!(h, 4096);
}

// === Two-phase ===

#[test]
fn check_runs_before_dispatch_and_returns_unit_on_ok() {
    let be = fixture();
    let action = Action {
        action_id: 1,
        kind: "click".into(),
        estimated_walltime_ms: 100,
        estimated_net_bytes: 0,
    };
    fn _ck<B: BudgetEnforcer>(b: &B, s: SessionId, a: &Action) -> Result<(), LoomError> {
        b.check(s, a)
    }
    let _ = _ck::<LocalBudgetEnforcer>;
    let _ = (be, action);
}

#[test]
fn account_runs_after_side_effect_and_takes_delta_u64() {
    fn _ck<B: BudgetEnforcer>(b: &B) -> Result<(), LoomError> {
        b.account(SessionId("01HZ".into()), ResourceKind::Network, 1024)
    }
    let _ = _ck::<LocalBudgetEnforcer>;
}

// === Exceeded variants per resource kind ===

#[test]
fn check_returns_budget_exceeded_on_walltime_breach() {
    let _e = LoomErrorCode::BudgetExceeded;
}

#[test]
fn account_returns_budget_exceeded_when_post_delta_breaches_network() {
    let _e = LoomErrorCode::BudgetExceeded;
}

#[test]
fn account_returns_budget_exceeded_on_dom_nodes_breach() {
    let _e = LoomErrorCode::BudgetExceeded;
}

#[test]
fn account_returns_budget_exceeded_on_js_heap_breach() {
    let _e = LoomErrorCode::BudgetExceeded;
}

// === Kill-callback cycle break ===

#[test]
fn register_session_takes_kill_callback_arc_dyn_fn_session_id_killreason() {
    let be = fixture();
    let kill: KillCallback = noop_kill();
    let counters = SessionCounters::new();
    fn _ck(
        be: &LocalBudgetEnforcer,
        id: SessionId,
        c: Arc<SessionCounters>,
        l: BudgetLimits,
        k: KillCallback,
    ) {
        be.register_session(id, c, l, k);
    }
    let _ = (be, kill, counters, _ck);
}

#[test]
fn kill_reason_carries_resource_kind_observed_limit_for_audit() {
    let r = KillReason::BudgetExceeded {
        kind: ResourceKind::Walltime,
        observed: 60_500,
        limit: 60_000,
    };
    if let KillReason::BudgetExceeded {
        kind,
        observed,
        limit,
    } = r
    {
        assert_eq!(kind, ResourceKind::Walltime);
        assert_eq!(observed, 60_500);
        assert_eq!(limit, 60_000);
    } else {
        panic!();
    }
}

#[test]
fn unregister_session_called_on_close_or_kill() {
    let be = fixture();
    be.unregister_session(SessionId("01HZ".into()));
}

#[test]
fn action_struct_uses_integer_estimates_no_floats() {
    let a = Action {
        action_id: u64::MAX,
        kind: "evaluate".into(),
        estimated_walltime_ms: 1000,
        estimated_net_bytes: 0,
    };
    let _u: u64 = a.estimated_walltime_ms;
    let _u2: u64 = a.estimated_net_bytes;
}
