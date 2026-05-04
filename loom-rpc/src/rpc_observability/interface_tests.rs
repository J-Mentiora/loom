// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-rpc/modules/rpc_observability/interface_tests.rs` instead.
// Interface tests for `RpcObservability`. Verifies AC-ARCH-38 span
// field set, SR-RPC-05 latency partition fields, redaction surface.

use super::rpc_observability::{
    RpcObservability, RpcObservabilityApi, RpcSpan, SpanGuard, ValidationOutcome,
};
use std::sync::Arc;

#[test]
fn rpc_span_has_required_ac_arch_38_fields() {
    fn _ck(s: &RpcSpan) {
        let _: &String = &s.request_id;
        let _: &String = &s.method_name;
        let _: &Option<String> = &s.session_id;
        let _: &u64 = &s.latency_us;
        let _: &ValidationOutcome = &s.validation_outcome;
        let _: &Option<String> = &s.error_code;
    }
    let _ = _ck;
}

#[test]
fn rpc_span_has_sr_rpc_05_latency_partition_fields() {
    fn _ck(s: &RpcSpan) {
        let _: &Option<u64> = &s.validation_us;
        let _: &Option<u64> = &s.framing_us;
        let _: &Option<u64> = &s.host_dispatch_us;
    }
    let _ = _ck;
}

#[test]
fn validation_outcome_enum_serialises_snake_case() {
    let s = serde_json::to_string(&ValidationOutcome::Pass).unwrap();
    assert_eq!(s, "\"pass\"");
    let s = serde_json::to_string(&ValidationOutcome::SchemaViolation).unwrap();
    assert_eq!(s, "\"schema_violation\"");
    let s = serde_json::to_string(&ValidationOutcome::MethodNotFound).unwrap();
    assert_eq!(s, "\"method_not_found\"");
}

#[test]
fn observability_api_has_start_span_signature() {
    fn _ck<O: RpcObservabilityApi>(o: &O, rid: String, m: &str) -> SpanGuard {
        o.start_span(rid, m)
    }
    let _ = _ck::<RpcObservability>;
}

#[test]
fn observability_api_supports_record_validation() {
    fn _ck<O: RpcObservabilityApi>(o: &O, g: &mut SpanGuard, v: ValidationOutcome) {
        o.record_validation(g, v)
    }
    let _ = _ck::<RpcObservability>;
}

#[test]
fn observability_api_supports_record_host_dispatch_separately() {
    // SR-RPC-05: action latency excluded from RPC budget.
    fn _ck<O: RpcObservabilityApi>(o: &O, g: &mut SpanGuard, us: u64) {
        o.record_host_dispatch(g, us)
    }
    let _ = _ck::<RpcObservability>;
}

#[test]
fn observability_api_supports_record_error_with_loom_error_code() {
    fn _ck<O: RpcObservabilityApi>(o: &O, g: &mut SpanGuard, code: &str) {
        o.record_error(g, code)
    }
    let _ = _ck::<RpcObservability>;
}

#[test]
fn observability_api_exposes_redact_for_vault_grant_inputs() {
    // AC-ARCH-38 redaction layer.
    fn _ck<O: RpcObservabilityApi>(o: &O, body: &mut serde_json::Value) {
        o.redact(body)
    }
    let _ = _ck::<RpcObservability>;
}

#[test]
fn span_guard_set_session_id_marks_session_field() {
    // Compile-time only — checks the API exists.
    fn _ck(g: &mut SpanGuard, sid: String) {
        g.set_session_id(sid)
    }
    let _ = _ck;
}

#[test]
fn observability_constructor_returns_arc() {
    fn _ck() -> Arc<RpcObservability> {
        RpcObservability::new()
    }
    let _ = _ck;
}

#[test]
fn observability_with_extra_redaction_keys_returns_arc() {
    fn _ck(extra: Vec<String>) -> Arc<RpcObservability> {
        RpcObservability::with_extra_redaction_keys(extra)
    }
    let _ = _ck;
}
