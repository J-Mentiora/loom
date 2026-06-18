// Budget enforcer behavior tests.
// Coverage:
//   - session wall-clock total kill (600s session_walltime_ms).
//   - per-action wall-clock pre-check (60s action_walltime_ms).
//   - cumulative network bytes kill (50MB).
//   - DOM node gauge kill (50k).
//   - JS heap gauge kill (512MB).
//   - manifest Header stores budget overrides (tested via manifest_writer).

use loom_core::budget_enforcer::{
    Action, BudgetEnforcer, BudgetLimits, KillCallback, KillReason, LocalBudgetEnforcer,
    ResourceKind, SessionCounters,
};
use loom_core::error::LoomErrorCode;
use loom_core::manifest_writer::{LocalManifestWriter, ManifestWriter, SessionId};
use loom_core::observability::Observability;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

fn obs() -> Arc<Observability> {
    // The budget enforcer is fully in-memory; Observability's `log_path` is never
    // written to disk by these tests (it's only ever stored). Hand it a throwaway
    // per-call TempDir path instead of a shared `/tmp` literal so nothing can
    // collide across runs — the dir need not outlive the call precisely because
    // nothing is written there.
    let tmp = TempDir::new().unwrap();
    Observability::new(tmp.path().join("loom.log"), false)
}

fn enforcer() -> LocalBudgetEnforcer {
    LocalBudgetEnforcer::new(obs())
}

fn sid(n: &str) -> SessionId {
    SessionId(format!("01HZ{:0>22}", n))
}

fn noop_kill() -> KillCallback {
    Arc::new(|_id, _reason| {})
}

fn capturing_kill() -> (KillCallback, Arc<Mutex<Vec<KillReason>>>) {
    let log: Arc<Mutex<Vec<KillReason>>> = Arc::new(Mutex::new(Vec::new()));
    let log2 = Arc::clone(&log);
    let cb: KillCallback = Arc::new(move |_id, reason| {
        log2.lock().unwrap().push(reason);
    });
    (cb, log)
}

fn register(
    be: &LocalBudgetEnforcer,
    id: SessionId,
    limits: BudgetLimits,
    kill: KillCallback,
) -> Arc<SessionCounters> {
    let counters = SessionCounters::new();
    be.register_session(id, Arc::clone(&counters), limits, kill);
    counters
}

fn action_with_estimate(walltime_ms: u64) -> Action {
    Action {
        action_id: 1,
        kind: "click".into(),
        estimated_walltime_ms: walltime_ms,
        estimated_net_bytes: 0,
    }
}

// ============================================================
// session wall-clock total kill
// ============================================================

#[test]
fn budget_01_1_session_walltime_kill_fires_callback() {
    let be = enforcer();
    let (kill, log) = capturing_kill();
    let limits = BudgetLimits {
        session_walltime_ms: 100,
        ..BudgetLimits::default()
    };
    register(&be, sid("A"), limits, kill);

    let result = be.account(sid("A"), ResourceKind::Walltime, 101);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().code, LoomErrorCode::BudgetExceeded);

    let kills = log.lock().unwrap();
    assert_eq!(kills.len(), 1, "kill callback must fire exactly once");
    if let KillReason::BudgetExceeded { kind, .. } = &kills[0] {
        assert_eq!(*kind, ResourceKind::Walltime);
    } else {
        panic!("unexpected kill reason: {:?}", kills[0]);
    }
}

#[test]
fn budget_01_1_kill_callback_not_double_fired_on_repeated_account() {
    let be = enforcer();
    let (kill, log) = capturing_kill();
    let limits = BudgetLimits {
        session_walltime_ms: 50,
        ..BudgetLimits::default()
    };
    register(&be, sid("B"), limits, kill);

    let _ = be.account(sid("B"), ResourceKind::Walltime, 51);
    let _ = be.account(sid("B"), ResourceKind::Walltime, 10); // second call post-breach
    let kills = log.lock().unwrap();
    assert_eq!(kills.len(), 1, "kill callback must be idempotent");
}

#[test]
fn budget_01_1_error_context_has_seconds_not_ms() {
    let be = enforcer();
    // 600_000ms = 600s session limit (default). Exceed by 1ms.
    register(&be, sid("C"), BudgetLimits::default(), noop_kill());
    // Accumulate 600_001 ms total.
    let _ = be.account(sid("C"), ResourceKind::Walltime, 600_001);
    let err = be.account(sid("C"), ResourceKind::Walltime, 1).unwrap_err();
    let ctx = err.context.as_ref().expect("error must have context");
    // Observed must be in seconds (≥ 600)
    let observed = ctx["observed"].as_u64().expect("observed must be u64");
    let limit = ctx["limit"].as_u64().expect("limit must be u64");
    assert!(
        observed >= 600,
        "observed={observed} must be >= 600 seconds"
    );
    assert_eq!(limit, 600, "limit must be 600 seconds");
    let budget_name = ctx["budget_name"]
        .as_str()
        .expect("budget_name must be string");
    assert_eq!(budget_name, "session_wall_clock_seconds");
}

#[test]
fn budget_01_1_check_rejects_if_session_already_at_limit() {
    let be = enforcer();
    let limits = BudgetLimits {
        session_walltime_ms: 100,
        ..BudgetLimits::default()
    };
    let counters = register(&be, sid("D"), limits, noop_kill());
    // Manually set the counter past the limit.
    counters.walltime_ms.store(100, Ordering::Relaxed);

    let result = be.check(sid("D"), &action_with_estimate(0));
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().code, LoomErrorCode::BudgetExceeded);
}

// ============================================================
// per-action wall-clock pre-check
// ============================================================

#[test]
fn budget_01_2_check_rejects_action_exceeding_action_limit() {
    let be = enforcer();
    // action_walltime_ms = 60_000 (default 60s). Pass 61s estimate.
    register(&be, sid("E"), BudgetLimits::default(), noop_kill());

    let action = action_with_estimate(61_000);
    let result = be.check(sid("E"), &action);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.code, LoomErrorCode::BudgetExceeded);
    let ctx = err.context.as_ref().expect("error must have context");
    let budget_name = ctx["budget_name"]
        .as_str()
        .expect("budget_name must be string");
    assert_eq!(budget_name, "action_wall_clock_seconds");
    let limit = ctx["limit"].as_u64().expect("limit must be u64");
    assert_eq!(limit, 60, "action limit must be 60 seconds");
}

#[test]
fn budget_01_2_session_remains_active_after_action_pre_rejection() {
    let be = enforcer();
    register(&be, sid("F"), BudgetLimits::default(), noop_kill());

    // Action that's too large — rejected pre-dispatch.
    let _ = be.check(sid("F"), &action_with_estimate(90_000));

    // Session is still active — a well-sized action check passes.
    let small_action = action_with_estimate(100);
    assert!(
        be.check(sid("F"), &small_action).is_ok(),
        "session must remain active after per-action rejection"
    );
}

// ============================================================
// cumulative network bytes kill
// ============================================================

#[test]
fn budget_01_3_network_bytes_kill_fires() {
    let be = enforcer();
    let (kill, log) = capturing_kill();
    register(&be, sid("G"), BudgetLimits::default(), kill);

    let result = be.account(sid("G"), ResourceKind::Network, 52_428_801);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().code, LoomErrorCode::BudgetExceeded);
    assert_eq!(log.lock().unwrap().len(), 1);
}

#[test]
fn budget_01_3_error_context_has_correct_keys() {
    let be = enforcer();
    register(&be, sid("H"), BudgetLimits::default(), noop_kill());

    let err = be
        .account(sid("H"), ResourceKind::Network, 52_428_801)
        .unwrap_err();
    let ctx = err.context.as_ref().expect("error must have context");
    let observed_bytes = ctx["observed_bytes"]
        .as_u64()
        .expect("observed_bytes must be u64");
    let limit_bytes = ctx["limit_bytes"]
        .as_u64()
        .expect("limit_bytes must be u64");
    assert!(observed_bytes >= 52_428_800);
    assert_eq!(
        limit_bytes, 52_428_800,
        "default network limit must be 50MB"
    );
}

// ============================================================
// DOM node gauge kill
// ============================================================

#[test]
fn budget_01_4_dom_nodes_gauge_kill_fires() {
    let be = enforcer();
    let (kill, log) = capturing_kill();
    register(&be, sid("I"), BudgetLimits::default(), kill);

    let result = be.account(sid("I"), ResourceKind::DomNodes, 50_001);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().code, LoomErrorCode::BudgetExceeded);
    assert_eq!(log.lock().unwrap().len(), 1);
}

#[test]
fn budget_01_4_dom_nodes_error_context_correct() {
    let be = enforcer();
    register(&be, sid("J"), BudgetLimits::default(), noop_kill());

    let err = be
        .account(sid("J"), ResourceKind::DomNodes, 50_001)
        .unwrap_err();
    let ctx = err.context.as_ref().expect("context must be present");
    assert_eq!(ctx["observed_nodes"].as_u64().unwrap(), 50_001);
    assert_eq!(ctx["limit_nodes"].as_u64().unwrap(), 50_000);
}

#[test]
fn budget_01_4_dom_nodes_are_gauge_not_cumulative() {
    let be = enforcer();
    // Two pages: 30k nodes each. Neither exceeds 50k. Cumulative would.
    register(&be, sid("K"), BudgetLimits::default(), noop_kill());

    assert!(be.account(sid("K"), ResourceKind::DomNodes, 30_000).is_ok());
    assert!(
        be.account(sid("K"), ResourceKind::DomNodes, 30_000).is_ok(),
        "two 30k-node pages must NOT trip the 50k limit (gauge, not cumulative)"
    );
}

// ============================================================
// JS heap gauge kill
// ============================================================

#[test]
fn budget_01_5_js_heap_gauge_kill_fires() {
    let be = enforcer();
    let (kill, log) = capturing_kill();
    register(&be, sid("L"), BudgetLimits::default(), kill);

    let result = be.account(sid("L"), ResourceKind::JsHeap, 536_870_913);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().code, LoomErrorCode::BudgetExceeded);
    assert_eq!(log.lock().unwrap().len(), 1);
}

#[test]
fn budget_01_5_js_heap_error_context_correct() {
    let be = enforcer();
    register(&be, sid("M"), BudgetLimits::default(), noop_kill());

    let err = be
        .account(sid("M"), ResourceKind::JsHeap, 536_870_913)
        .unwrap_err();
    let ctx = err.context.as_ref().expect("context must be present");
    assert_eq!(ctx["observed_heap_bytes"].as_u64().unwrap(), 536_870_913);
    assert_eq!(ctx["limit_heap_bytes"].as_u64().unwrap(), 536_870_912);
}

#[test]
fn budget_01_5_js_heap_is_gauge_not_cumulative() {
    let be = enforcer();
    // Heap fluctuates: 400MB, then 200MB. Neither exceeds 512MB.
    register(&be, sid("N"), BudgetLimits::default(), noop_kill());

    assert!(be
        .account(sid("N"), ResourceKind::JsHeap, 400 * 1024 * 1024)
        .is_ok());
    assert!(
        be.account(sid("N"), ResourceKind::JsHeap, 200 * 1024 * 1024)
            .is_ok(),
        "heap drop must not cause false breach (gauge not cumulative)"
    );
}

// ============================================================
// Below-limit — no error
// ============================================================

#[test]
fn budget_below_limit_all_resources_ok() {
    let be = enforcer();
    register(&be, sid("O"), BudgetLimits::default(), noop_kill());

    assert!(be.account(sid("O"), ResourceKind::Walltime, 1_000).is_ok());
    assert!(be.account(sid("O"), ResourceKind::Network, 1_024).is_ok());
    assert!(be.account(sid("O"), ResourceKind::DomNodes, 100).is_ok());
    assert!(be
        .account(sid("O"), ResourceKind::JsHeap, 1024 * 1024)
        .is_ok());
}

// ============================================================
// Session lifecycle — register / unregister
// ============================================================

#[test]
fn unregister_removes_session_check_returns_not_found() {
    let be = enforcer();
    register(&be, sid("P"), BudgetLimits::default(), noop_kill());
    be.unregister_session(sid("P"));

    let result = be.check(sid("P"), &action_with_estimate(0));
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().code, LoomErrorCode::SessionNotFound);
}

#[test]
fn unregister_unknown_session_is_noop() {
    let be = enforcer();
    // Should not panic.
    be.unregister_session(sid("Q"));
}

// ============================================================
// manifest Header stores budget overrides
// ============================================================

#[test]
fn budget_04_1_budget_overrides_stored_in_manifest_header() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let obs = Observability::new(root.join("loom.log"), false);
    let mw = LocalManifestWriter::new(root.to_path_buf(), obs);

    let id = sid("BUDGET04");
    let limits = BudgetLimits {
        session_walltime_ms: 30_000, // wall_clock=30s
        action_walltime_ms: 60_000,
        network_bytes: 10 * 1024 * 1024, // network=10MB
        dom_nodes: 50_000,
        js_heap_bytes: 512 * 1024 * 1024,
    };
    mw.open_manifest(id.clone(), Some(limits)).unwrap();

    // Read the WAL back and verify the Header entry has the budget fields.
    let wal_path = root.join(&id.0).join("manifest.wal");
    let content = std::fs::read_to_string(&wal_path).unwrap();
    let first_line = content.lines().next().unwrap();
    let entry: serde_json::Value = serde_json::from_str(first_line).unwrap();

    assert_eq!(entry["kind"], "header");
    let budgets = entry["budgets"]
        .as_object()
        .expect("budgets field must be present");
    assert_eq!(
        budgets["session_walltime_ms"].as_u64().unwrap(),
        30_000,
        "session_walltime_ms must match override"
    );
    assert_eq!(
        budgets["network_bytes"].as_u64().unwrap(),
        10 * 1024 * 1024,
        "network_bytes must match override"
    );
}

// Note: CLI parse_budget_string tests live in loom-cli's session_commands interface_tests.
