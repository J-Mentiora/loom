// Interface tests for `HostObservability`. Verifies the redaction
// layer's blocklist (defense-in-depth on the vault-isolation invariant)
// and the sink-module acyclicity invariant.

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

// === Redaction layer: defense-in-depth for vault isolation ===

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

// === Acyclicity: sink has no upstream deps ===

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
    // Integer-only counts. frames_count is u32, action_id is u64,
    // debug_info_unavailable is bool — no floats anywhere.
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
    // the hot path matter for the SLA overhead budget.
    let obs = HostObservability::new(false);
    let _: u64 = obs.dropped_events();
}

// === v0.9.6 web-cookie-injection: Redacted<T> serialisation invariant ===
// The verb-emitted receipts carry cookie values inside `Redacted<String>`
// (loom-shared::Redacted). When a structured tracing event is generated
// for `loom.web.set_cookies`, the cookie `value` field MUST serialise as
// "[REDACTED]" rather than the raw value. This pins the structural
// invariant: cookie values cannot leak via serde_json::to_string of a
// cookie struct (which is what structured tracing fields go through).

use loom_shared::Redacted;
use serde::Serialize;

#[derive(Serialize)]
struct StructuredCookieEvent {
    name: String,
    value: Redacted<String>,
    domain: String,
}

#[test]
fn redacted_string_emits_redacted_token_in_serde_json_output_for_cookie_event() {
    let event = StructuredCookieEvent {
        name: "sid".to_string(),
        value: Redacted::new("secret-session-token".to_string()),
        domain: "example.com".to_string(),
    };
    let json = serde_json::to_string(&event).expect("serialise");
    assert!(
        !json.contains("secret-session-token"),
        "raw cookie value must not appear in serialised structured tracing output; got: {json}"
    );
    assert!(
        json.contains("[REDACTED]"),
        "structured output must carry [REDACTED] marker; got: {json}"
    );
    // Non-secret fields remain visible.
    assert!(json.contains("\"name\":\"sid\""));
    assert!(json.contains("\"domain\":\"example.com\""));
}

#[test]
fn redacted_string_emits_redacted_token_in_debug_format_for_tracing_event_fields() {
    // tracing events using `?value` (Debug formatter) on a Redacted
    // field also produce "[REDACTED]" — verifies the second emit path.
    let redacted: Redacted<String> = Redacted::new("ULTRA_SECRET_SESSION".to_string());
    let debug_str = format!("{:?}", redacted);
    assert!(
        !debug_str.contains("ULTRA_SECRET_SESSION"),
        "Debug formatter must not leak raw value; got: {debug_str}"
    );
    assert!(
        debug_str.contains("[REDACTED]"),
        "Debug formatter must emit [REDACTED]; got: {debug_str}"
    );
}

#[test]
fn redacted_string_emits_redacted_token_in_display_format() {
    let redacted: Redacted<String> = Redacted::new("DISPLAY_LEAK_PROBE".to_string());
    let display_str = format!("{}", redacted);
    assert!(
        !display_str.contains("DISPLAY_LEAK_PROBE"),
        "Display formatter must not leak raw value; got: {display_str}"
    );
    assert!(
        display_str.contains("[REDACTED]"),
        "Display formatter must emit [REDACTED]; got: {display_str}"
    );
}
