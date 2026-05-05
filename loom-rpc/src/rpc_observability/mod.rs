//! `rpc_observability` — see crate root.
pub mod rpc_observability;
pub use rpc_observability::*;

#[cfg(test)]
mod interface_tests;

use std::time::Instant;

impl RpcObservabilityApi for RpcObservability {
    fn start_span(&self, request_id: String, method: &str) -> SpanGuard {
        SpanGuard {
            span: RpcSpan {
                request_id,
                method_name: method.to_string(),
                session_id: None,
                latency_us: 0,
                validation_outcome: ValidationOutcome::Pass,
                error_code: None,
                validation_us: None,
                framing_us: None,
                host_dispatch_us: None,
            },
            start: Instant::now(),
        }
    }

    fn record_validation(&self, guard: &mut SpanGuard, outcome: ValidationOutcome) {
        guard.span.validation_outcome = outcome;
        let elapsed = guard.start.elapsed();
        guard.span.validation_us = Some(elapsed.as_micros() as u64);
    }

    fn record_host_dispatch(&self, guard: &mut SpanGuard, dispatch_us: u64) {
        guard.span.host_dispatch_us = Some(dispatch_us);
    }

    fn record_error(&self, guard: &mut SpanGuard, code: &str) {
        guard.span.error_code = Some(code.to_string());
    }

    fn redact(&self, body: &mut serde_json::Value) {
        redact_recursive(body, &self.redaction_keys);
    }
}

fn redact_recursive(val: &mut serde_json::Value, keys: &[String]) {
    match val {
        serde_json::Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if keys.iter().any(|rk| rk.eq_ignore_ascii_case(k)) {
                    *v = serde_json::Value::String("<redacted>".to_string());
                } else {
                    redact_recursive(v, keys);
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr.iter_mut() {
                redact_recursive(v, keys);
            }
        }
        _ => {}
    }
}
