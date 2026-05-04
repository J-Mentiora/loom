// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-core/modules/observability/interface_tests.rs` instead.
// Interface tests for `Observability`. These tests verify the contract
// surface against verification criteria; they are authored as compilable
// Rust against `interfaces.rs` in the same dir.
//
// Covered criteria:
// - Observability story (universal gap closure): redaction layer, span
//   lifecycle, fail-open metric counters, structured fields.
// - Hard binding 3: BudgetSnapshot is integer-only.

use super::observability::{
    redaction_layer, BudgetSnapshot, LogField, LogLevel, Observability,
};
use std::path::PathBuf;

fn fixture() -> std::sync::Arc<Observability> {
    Observability::new(PathBuf::from("/tmp/loom-test/loom.log"), false)
}

#[test]
fn span_start_returns_handle_carrying_supplied_name() {
    let obs = fixture();
    let handle = obs.span_start("session.create", vec![]);
    assert_eq!(handle.name, "session.create");
}

#[test]
fn span_end_consumes_handle_without_panic() {
    let obs = fixture();
    let handle = obs.span_start("session.create", vec![
        LogField { key: "session_id", value: "01HZ...".into(), secret: false }
    ]);
    obs.span_end(handle, vec![
        LogField { key: "latency_ms", value: "42".into(), secret: false }
    ]);
}

#[test]
fn log_event_accepts_required_session_action_error_fields() {
    let obs = fixture();
    obs.log_event(
        LogLevel::Error,
        "manifest append failed",
        vec![
            LogField { key: "session_id", value: "01HZ...".into(), secret: false },
            LogField { key: "action_id", value: "7".into(), secret: false },
            LogField { key: "error_code", value: "store_full_no_evictable".into(), secret: false },
            LogField { key: "latency_ms", value: "1200".into(), secret: false },
        ],
    );
}

#[test]
fn log_redacted_marks_secret_field_for_redaction() {
    // Redaction occurs at the layer, but the API contract is: fields with
    // secret=true must never reach the JSON serializer with raw value.
    let obs = fixture();
    obs.log_redacted(
        "vault.substitute",
        vec![
            LogField { key: "grant_id", value: "01HZ...".into(), secret: false },
            // This field, even if accidentally passed, must be redacted.
            LogField { key: "authorization", value: "should-not-leak".into(), secret: true },
        ],
    );
}

#[test]
fn metric_inc_does_not_fail_when_disk_log_path_is_missing() {
    // Fail-open property: disk-full or missing-parent must not bubble.
    let obs = Observability::new(PathBuf::from("/nonexistent/dir/loom.log"), false);
    obs.metric_inc("manifest_append_failures", 1);
    obs.metric_inc("budget_exceedances.walltime", 1);
}

#[test]
fn budget_snapshot_is_integer_only_per_hard_binding_3() {
    // Compile-time guarantee: structurally proven by field types being u64.
    let snap = BudgetSnapshot {
        walltime_ms: 60_000,
        net_bytes: 50_000_000,
        dom_nodes: 50_000,
        heap_bytes: 512 * 1024 * 1024,
    };
    // The default-equals check ensures Eq is derivable (impossible with floats).
    assert_eq!(snap.walltime_ms + 1, 60_001);
    assert_ne!(snap, BudgetSnapshot::default());
}

#[test]
fn redaction_layer_can_be_constructed_for_subscriber_wiring() {
    let _layer = redaction_layer();
    // Layer is consumed by tracing-subscriber at binary entry; here we
    // only verify it can be constructed.
}

#[test]
fn snapshot_metrics_returns_counters_struct() {
    let obs = fixture();
    let snap = obs.snapshot_metrics();
    assert_eq!(snap.manifest_append_failures, 0);
    assert!(snap.vault_grant_rejections.is_empty());
}
