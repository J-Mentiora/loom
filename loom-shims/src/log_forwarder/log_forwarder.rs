// LogForwarder — Chromium stdout/stderr → daemon tracing pipeline.
//
// # Contract semantics
// - **Fail-open.** Log emission failures are SWALLOWED — broken pipes
//   and full channels increment a drop counter and continue. The shim
//   must never block on logging; an OOM in the log channel must not
//   cascade into a Chromium-supervision pause.
// - **Structured tracing emission.** Each parsed line becomes a
//   `ShimResponse::LogLine{level, target, message}`. The daemon's
//   `ShimManager` correlates by `target=loom_shim_chromium::log` +
//   timestamp.
// - **No private payload retention.** Lines are forwarded
//   verbatim — `LogForwarder` does NOT redact or mutate. Chromium's
//   own log redaction is the upstream concern.
// - **Critical-line escalation.** Lines containing
//   `"FATAL:"` / `"CHECK failed:"` / `"chromium crashed"` are tagged
//   `level=error` and additionally trigger
//   `Supervisor::handle_crash` via the registered callback (so the
//   daemon hears about a Chromium-internal CHECK before the SIGCHLD
//   races).

use crate::ipc_endpoint::ipc_endpoint::ResponseSender;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

/// Type alias to reduce complexity of the critical-callback field.
pub type CriticalCallback = Arc<dyn Fn(String) + Send + Sync>;

/// Drop counter — incremented when emission fails (fail-open semantics).
#[derive(Debug, Default)]
pub struct ForwarderCounters {
    pub lines_forwarded: AtomicU64,
    pub lines_dropped_channel_full: AtomicU64,
    pub lines_dropped_pipe_broken: AtomicU64,
}

impl ForwarderCounters {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

/// Concrete log forwarder.
pub struct ChromiumLogForwarder {
    pub(crate) response_tx: ResponseSender,
    pub(crate) counters: Arc<ForwarderCounters>,
    pub(crate) critical_callback: parking_lot::RwLock<Option<CriticalCallback>>,
}

impl ChromiumLogForwarder {
    pub fn new(response_tx: ResponseSender) -> Self {
        Self {
            response_tx,
            counters: ForwarderCounters::new(),
            critical_callback: parking_lot::RwLock::new(None),
        }
    }
}

/// Public LogForwarder trait surface.
pub trait LogForwarder: Send + Sync {
    /// Spawn the stdout / stderr pump tasks. The pumps read from the
    /// pipe FDs returned by `Supervisor` at child spawn.
    /// Errors: `ForwarderError::PipeUnavailable` if the FDs are
    /// closed.
    fn spawn_pumps(&self, stdout_fd: i32, stderr_fd: i32) -> Result<(), ForwarderError>;

    /// Register the critical-line escalation callback. Invoked by the
    /// pump on lines matching the critical patterns.
    fn register_critical_callback(&self, cb: Arc<dyn Fn(String) + Send + Sync>);

    /// Snapshot drop counters.
    fn counters(&self) -> Arc<ForwarderCounters>;

    /// Drain pumps cooperatively at shutdown.
    fn shutdown(&self) -> Result<(), ForwarderError>;
}

#[derive(Debug, thiserror::Error)]
pub enum ForwarderError {
    #[error("pipe FDs unavailable")]
    PipeUnavailable,
    #[error("pumps already spawned")]
    AlreadySpawned,
}

impl LogForwarder for ChromiumLogForwarder {
    fn spawn_pumps(&self, _stdout_fd: i32, _stderr_fd: i32) -> Result<(), ForwarderError> {
        // pipe FD pump tasks — not yet implemented
        Ok(())
    }

    fn register_critical_callback(&self, cb: Arc<dyn Fn(String) + Send + Sync>) {
        *self.critical_callback.write() = Some(cb);
    }

    fn counters(&self) -> Arc<ForwarderCounters> {
        Arc::clone(&self.counters)
    }

    fn shutdown(&self) -> Result<(), ForwarderError> {
        Ok(())
    }
}

/// Pure helper: classify a Chromium log line.
/// `(level, is_critical)`.
pub fn classify_line(line: &str) -> (LogLevel, bool) {
    let lower = line.to_lowercase();
    let is_critical = lower.contains("fatal:")
        || lower.contains("check failed:")
        || lower.contains("chromium crashed");
    let level = if is_critical || lower.contains("error") || lower.contains("err]") {
        LogLevel::Error
    } else if lower.contains("warn") {
        LogLevel::Warn
    } else if lower.contains("info") {
        LogLevel::Info
    } else if lower.contains("debug") || lower.contains("verbose") {
        LogLevel::Debug
    } else {
        LogLevel::Info
    };
    (level, is_critical)
}

/// Log level enum. Maps 1:1 to tracing's levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }
}
