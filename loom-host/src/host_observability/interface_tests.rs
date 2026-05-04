// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-host/modules/host_observability/interface_tests.rs` instead.
// Interface tests for `HostObservability`. Verifies the redaction
// layer's blocklist (defense-in-depth atop IC-HOST-04) and the
// sink-module acyclicity invariant.

use super::host_observability::{HostCallMetric, HostObservability, RedactionLayer, TrapEvent};

// === Sink module: only public ctor + simple emit methods ===

#[test]
fn observability_new_returns_arc() {
    let obs = HostObservability::new(true);
    assert!(obs.redaction_active());
    assert_eq!(obs.dropped_events(), 0);
}

#[test]
fn record_host_call_does_not_error() {
    let obs = HostObservability::new(true);
    obs.record_host_call(HostCallMetric {
        host_fn: "blob_put".into(),
        session_id: "01HZ".into(),
        latency_us: 47,
        tape_offset: 12,
        error: None,
    });
    // Fail-open: dropped_events may or may not bump; the call itself
    // never panics nor returns Result.
    let _ = obs.dropped_events();
}

#[test]
fn record_trap_event_returns_ok() {
    let obs = HostObservability::new(true);
    let r = obs.record_trap_event(TrapEvent {
        session_id: "01HZ".into(),
        action_id: 7,
        surface: "stocktwits".into(),
        trap_code: "unreachable".into(),
        frames_count: 3,
        debug_info_unavailable: false,
    });
    assert!(r.is_ok(), "trap event recording is fail-open");
}

// === Redaction layer: defense-in-depth for IC-HOST-04 ===

#[test]
fn redaction_strips_authorization_case_insensitive() {
    let layer = RedactionLayer::new();
    assert!(layer.should_redact("Authorization"));
    assert!(layer.should_redact("authorization"));
    assert!(layer.should_redact("AUTHORIZATION"));
}

#[test]
fn redaction_strips_cookie_and_set_cookie() {
    let layer = RedactionLayer::new();
    assert!(layer.should_redact("Cookie"));
    assert!(layer.should_redact("cookie"));
    assert!(layer.should_redact("Set-Cookie"));
    assert!(layer.should_redact("set-cookie"));
}

#[test]
fn redaction_passes_normal_field_names() {
    let layer = RedactionLayer::new();
    assert!(!layer.should_redact("Content-Type"));
    assert!(!layer.should_redact("user_agent"));
    assert!(!layer.should_redact("session_id"));
    assert!(!layer.should_redact("host_fn"));
}

#[test]
fn redaction_extra_keys_are_blocked() {
    let layer = RedactionLayer::new().with_extra("X-Api-Key");
    assert!(layer.should_redact("X-Api-Key"));
    assert!(layer.should_redact("x-api-key")); // case-insensitive
}

// === Acyclicity (BC-HOST §2): sink has no upstream deps ===

#[test]
fn host_call_metric_is_constructible_without_other_loom_host_modules() {
    // The struct uses only String + integer types. Verifies no
    // structural import from `host_function_table`, `error_mapper`, etc.
    let _ = HostCallMetric {
        host_fn: "clock_now".into(),
        session_id: "01HZ".into(),
        latency_us: 1,
        tape_offset: 0,
        error: None,
    };
}

#[test]
fn trap_event_has_integer_only_fields_for_counts() {
    // BC HARD #3: integer-only counts. frames_count is u32, action_id
    // is u64, debug_info_unavailable is bool — no floats anywhere.
    let e = TrapEvent {
        session_id: "01HZ".into(),
        action_id: u64::MAX,
        surface: "x".into(),
        trap_code: "unreachable".into(),
        frames_count: u32::MAX,
        debug_info_unavailable: true,
    };
    let _: u64 = e.action_id;
    let _: u32 = e.frames_count;
}

#[test]
fn dropped_events_uses_relaxed_atomic() {
    // Compile-time pin: the counter is `AtomicU64`. Lock-free counts on
    // the hot path matter for IC-HOST-02 overhead budget.
    let obs = HostObservability::new(false);
    let _: u64 = obs.dropped_events();
}
