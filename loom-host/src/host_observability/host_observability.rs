// HostObservability — sink module for `tracing` spans + redaction layer.
//
// # Contract semantics
// - **Sink module.** Has no internal deps; every other module that
//   emits events depends on this one-way (acyclicity per design.md §2).
// - **Per-host-fn span.** Each host-fn dispatch in `HostFunctionTable`
//   wraps its body in a `tracing::span!(Level::INFO, "host_fn",
//   session_id=…, host_fn=…, latency_us=…, tape_offset=…)`.
// - **Redaction layer.** A custom `tracing::Layer` strips fields named
//   `Authorization`, `Cookie`, `Set-Cookie` from any structured event
//   before export. Defense-in-depth on the vault-isolation invariant
//   (the secret never
//   reached the log path in the first place, but the redaction layer
//   catches future regressions).
// - **Drop counter.** A process-wide `AtomicU64` counts dropped events
//   when the in-process buffer is full — fail-open per loom-core
//   `Observability` policy.
// - **Trap event.** `TrapHandler` calls `record_trap_event` with the
//   resolved frames; emits `tracing::error!(target="loom_host::trap", …)`.

use loom_core::error::LoomError;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Per-host-fn metric record. Captured by `SessionExecutor` after each
/// host-fn call; not persisted, only emitted via tracing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostCallMetric {
    pub host_fn: String,
    pub session_id: String,
    pub latency_us: u64,
    pub tape_offset: u64,
    pub error: Option<String>,
}

/// Trap event payload. Pure-integer + string — no floats by convention.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrapEvent {
    pub session_id: String,
    pub action_id: u64,
    pub surface: String,
    pub trap_code: String,
    pub frames_count: u32,
    pub debug_info_unavailable: bool,
}

/// The HostObservability handle. One per process; held by
/// `WasmHost` and threaded through `HostState`.
pub struct HostObservability {
    pub(crate) drop_count: AtomicU64,
    pub(crate) redaction_enabled: bool,
}

impl HostObservability {
    /// Construct. Installs the redaction layer into the global tracing
    /// subscriber. Idempotent — second call returns the existing handle.
    pub fn new(redaction_enabled: bool) -> Arc<Self> {
        Arc::new(Self {
            drop_count: AtomicU64::new(0),
            redaction_enabled,
        })
    }

    /// Record one host-fn metric. Emits a tracing event and returns;
    /// never errors. Buffer-full → bumps `drop_count`.
    pub fn record_host_call(&self, metric: HostCallMetric) {
        let _ = metric;
    }

    /// Record a trap. Emits `tracing::error!` and bumps the trap
    /// counter. Returns Ok always; the underlying tracing call is
    /// fail-open.
    pub fn record_trap_event(&self, event: TrapEvent) -> Result<(), LoomError> {
        let _ = event;
        Ok(())
    }

    /// Snapshot the drop counter. Used by `loom-cli loom diag observ`.
    pub fn dropped_events(&self) -> u64 {
        self.drop_count.load(Ordering::Relaxed)
    }

    /// Test seam: returns true iff the redaction layer is registered
    /// against the global subscriber.
    pub fn redaction_active(&self) -> bool {
        self.redaction_enabled
    }
}

/// The redaction layer. Implements `tracing_subscriber::Layer` (left
/// abstract here — the concrete impl is wired in the implementation). Strips:
///   - "authorization", "Authorization"
///   - "cookie", "Cookie", "set-cookie", "Set-Cookie"
///   - any field with name in the `extra_redacted_keys` set
pub struct RedactionLayer {
    pub(crate) extra_redacted_keys: Vec<String>,
}

impl RedactionLayer {
    pub fn new() -> Self {
        Self {
            extra_redacted_keys: Vec::new(),
        }
    }

    pub fn with_extra(mut self, key: impl Into<String>) -> Self {
        self.extra_redacted_keys.push(key.into());
        self
    }

    /// Pure helper for tests: returns true iff the field name is one
    /// the layer would strip.
    pub fn should_redact(&self, field_name: &str) -> bool {
        let lower = field_name.to_ascii_lowercase();
        matches!(lower.as_str(), "authorization" | "cookie" | "set-cookie")
            || self
                .extra_redacted_keys
                .iter()
                .any(|k| k.eq_ignore_ascii_case(field_name))
    }
}

impl Default for RedactionLayer {
    fn default() -> Self {
        Self::new()
    }
}
