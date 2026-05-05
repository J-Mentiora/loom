// Re-export of the locked Phase 5.3 interface. DO NOT EDIT here.
// Edit `systems/loom-rpc/modules/rpc_observability/interfaces.rs` instead.
// RpcObservability — `tracing` spans per RPC method + redaction layer.
//
// # Contract semantics
// - **Span fields per RPC (AC-ARCH-38).** `request_id`, `method_name`,
//   `session_id?`, `latency_us`, `validation_outcome`, `error_code?`.
//   Emitted as JSON via the `tracing` subscriber.
// - **Latency partition (SR-RPC-05 / IC-RPC-08).** `validation_us`,
//   `framing_us`, and `host_dispatch_us` are recorded as separate
//   span fields so the IC-RPC-08 budget is measured against
//   roundtrip overhead, NOT host action work.
// - **Redaction layer.** `vault.grant` request fields (`secret`,
//   `token`, `value`, `credential`, `password`) are scrubbed BEFORE
//   the span is emitted (BC-CORE-02 leak prevention in logs).
// - **Best-effort.** Span emission MUST NEVER block the request
//   path. Subscriber failures are dropped silently.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationOutcome {
    Pass,
    SchemaViolation,
    MethodNotFound,
}

/// One span's structured fields (AC-ARCH-38). `None`-valued fields
/// are omitted from the JSON output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcSpan {
    pub request_id: String,
    pub method_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub latency_us: u64,
    pub validation_outcome: ValidationOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    // SR-RPC-05 latency partition
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation_us: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub framing_us: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_dispatch_us: Option<u64>,
}

/// Trait surface for observability so `RpcHandlers` / `AuthMiddleware` /
/// `ErrorTranslator` can be unit-tested with a fake.
pub trait RpcObservabilityApi: Send + Sync {
    /// Begin a new span; returns a guard that records latency on drop.
    fn start_span(&self, request_id: String, method: &str) -> SpanGuard;

    /// Mark validation outcome on the active span.
    fn record_validation(&self, guard: &mut SpanGuard, outcome: ValidationOutcome);

    /// Stamp the host-dispatch interval (excluded from RPC budget).
    fn record_host_dispatch(&self, guard: &mut SpanGuard, dispatch_us: u64);

    /// Record an error code mid-span. Caller passes the
    /// `LoomErrorCode` snake_case name.
    fn record_error(&self, guard: &mut SpanGuard, code: &str);

    /// Redact a `serde_json::Value` request body before it is logged.
    /// Walks the tree; replaces values for keys in the redaction list
    /// with `"<redacted>"`. Pure function; never blocks.
    fn redact(&self, body: &mut serde_json::Value);
}

/// RAII guard tied to one in-flight RPC. Records `latency_us` on
/// `Drop` and emits the span via the configured tracing subscriber.
pub struct SpanGuard {
    pub(crate) span: RpcSpan,
    pub(crate) start: std::time::Instant,
}

impl SpanGuard {
    /// Set `session_id` once the request has been parsed and the
    /// handler knows which session this RPC targets.
    pub fn set_session_id(&mut self, session_id: String) {
        self.span.session_id = Some(session_id);
    }
}

/// Concrete implementation. Wraps a `tracing` subscriber configured
/// at daemon startup (BC-RPC-04: log level from config / env / CLI).
pub struct RpcObservability {
    pub(crate) redaction_keys: Vec<String>,
}

impl RpcObservability {
    /// Build with the default redaction list (`secret`, `token`,
    /// `value`, `credential`, `password`). Additional keys may be
    /// appended per-deploy via `with_extra_redaction_keys`.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            redaction_keys: default_redaction_keys(),
        })
    }

    pub fn with_extra_redaction_keys(mut keys: Vec<String>) -> Arc<Self> {
        let mut all = default_redaction_keys();
        all.append(&mut keys);
        Arc::new(Self {
            redaction_keys: all,
        })
    }
}

impl Default for RpcObservability {
    fn default() -> Self {
        Self {
            redaction_keys: default_redaction_keys(),
        }
    }
}

fn default_redaction_keys() -> Vec<String> {
    vec![
        "secret".into(),
        "token".into(),
        "value".into(),
        "credential".into(),
        "password".into(),
    ]
}
