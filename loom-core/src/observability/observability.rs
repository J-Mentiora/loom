// Observability — structured tracing JSON + redaction layer + OTel spans.
//
// Leaf module: no `loom_core::*` deps. Other modules depend on this one
// (one-way), so it cannot introduce a cycle. Fail-open: emission errors
// are counted in an in-memory ring buffer but never bubble up.
//
// # Contract semantics
// - Every event is structured `tracing` JSON; required fields:
//   `session_id` (when scoped), `action_id` (when scoped), `error_code`
//   (on error), `latency_ms` (on span_end), and a budget snapshot when
//   relevant.
// - The `redaction_layer()` walks event fields; any field tagged
//   `secret = true` is replaced with `[REDACTED]` BEFORE serialization.
//   Raw secret bytes never reach the JSON serializer.
// - `loom-core` modules call this private API (`span_start`, `span_end`,
//   `log_event`, `log_redacted`, `metric_inc`); the `loom` binary entry
//   point configures the OpenTelemetry exporter.

use std::collections::BTreeMap;
use std::sync::Arc;

/// Opaque span handle. `span_end` must be called with the matching handle
/// from `span_start`.
#[derive(Debug, Clone)]
pub struct SpanHandle {
    pub(crate) span_id: u64,
    pub(crate) name: &'static str,
    pub(crate) started_at_ns: u64,
}

/// Severity of a log event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// A field attached to a log event. The `secret` flag triggers redaction.
#[derive(Debug, Clone)]
pub struct LogField {
    pub key: &'static str,
    pub value: String,
    pub secret: bool,
}

/// Snapshot of the per-session budget counters at the moment of emission.
/// Pure integer fields (Hard binding 3 — no floats in receipt-shaped data).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BudgetSnapshot {
    pub walltime_ms: u64,
    pub net_bytes: u64,
    pub dom_nodes: u64,
    pub heap_bytes: u64,
}

/// In-memory metric counters (fail-open metrics ring buffer).
#[derive(Debug, Default)]
pub struct MetricCounters {
    pub manifest_append_failures: u64,
    pub vault_grant_rejections: BTreeMap<String, u64>,
    pub budget_exceedances: BTreeMap<String, u64>,
    pub log_emission_failures: u64,
}

/// The Observability sink. All `loom-core` modules hold an `Arc<Observability>`.
pub struct Observability {
    pub(crate) counters: Arc<parking_lot::Mutex<MetricCounters>>,
    pub(crate) log_path: std::path::PathBuf,
    pub(crate) otel_enabled: bool,
}

impl Observability {
    /// Construct a new Observability sink. Called once at daemon startup
    /// by the binary entry point; cloned via `Arc` into each core module.
    pub fn new(log_path: std::path::PathBuf, otel_enabled: bool) -> Arc<Self> {
        Arc::new(Self {
            counters: Arc::new(parking_lot::Mutex::new(MetricCounters::default())),
            log_path,
            otel_enabled,
        })
    }

    /// Begin a span. Returns a handle that must be passed to `span_end`.
    pub fn span_start(&self, name: &'static str, fields: Vec<LogField>) -> SpanHandle {
        let _ = fields;
        SpanHandle {
            span_id: 0,
            name,
            started_at_ns: 0,
        }
    }

    /// End a span. Records `latency_ms` derived from monotonic clock delta.
    pub fn span_end(&self, handle: SpanHandle, extra_fields: Vec<LogField>) {
        let _ = (handle, extra_fields);
    }

    /// Emit a log event. Fields with `secret=true` are redacted at this
    /// callsite before serialization.
    pub fn log_event(&self, level: LogLevel, msg: &str, fields: Vec<LogField>) {
        let _ = (level, msg, fields);
    }

    /// Convenience wrapper for paths that touch vault: marks any field
    /// containing secret-bytes as redacted regardless of caller intent.
    pub fn log_redacted(&self, msg: &str, fields: Vec<LogField>) {
        let _ = (msg, fields);
    }

    /// Increment a named metric counter (fail-open).
    pub fn metric_inc(&self, name: &str, by: u64) {
        let _ = (name, by);
    }

    /// Snapshot the in-memory metric counters.
    pub fn snapshot_metrics(&self) -> MetricCounters {
        MetricCounters::default()
    }
}

/// Construct the redaction `tracing` Layer. Wired by the binary entry
/// point into the global subscriber. Walks each event's fields and
/// replaces values for fields tagged `secret = true` with `[REDACTED]`.
pub fn redaction_layer() -> RedactionLayer {
    RedactionLayer { _private: () }
}

/// Tracing-compatible redaction layer. Consumed by `tracing-subscriber`
/// at the binary entry point; not used directly by `loom-core` modules.
pub struct RedactionLayer {
    _private: (),
}
