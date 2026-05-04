// ProcessMonitor — Chromium liveness watchdog.
//
// # Contract semantics
// - **Periodic ping (SR-SHIM-04).** Issues `Browser.getVersion` via
//   `CdpConnection` every 30s (soft default; tunable).
// - **Hang detection.** 3 consecutive timeouts → invokes the
//   registered `ForceRestartCallback`. The callback is owned by
//   `Supervisor`; `ProcessMonitor` does NOT import Supervisor —
//   one-way callback edge (acyclicity guarantee, design §2 Callback
//   patterns).
// - **Self-suspending on crash.** When the callback fires,
//   `ProcessMonitor` waits for `notify_resumed()` from `Supervisor`
//   before resuming pings. Avoids cascading callback storms during
//   restart.
// - **Lock-free counters.** `consecutive_timeouts` and `last_ping`
//   are atomic so the dispatcher loop can read them without
//   contention.

use crate::cdp_connection::cdp_connection::CdpConnection;
use crate::supervisor::supervisor::ForceRestartCallback;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Default ping cadence (soft binding).
pub const DEFAULT_PING_INTERVAL: Duration = Duration::from_secs(30);

/// Default consecutive-timeout threshold for hang detection.
pub const DEFAULT_TIMEOUT_THRESHOLD: u8 = 3;

/// Lock-free shared state. Public so observability code can sample
/// without acquiring locks.
#[derive(Debug, Default)]
pub struct MonitorCounters {
    pub consecutive_timeouts: AtomicU8,
    /// Unix-epoch millis of last successful ping. 0 = never.
    pub last_ping_unix_ms: AtomicU64,
    /// Whether the monitor is currently suspended (post-callback,
    /// pre-`notify_resumed`).
    pub suspended: std::sync::atomic::AtomicBool,
}

impl MonitorCounters {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

/// Concrete monitor.
pub struct ChromiumProcessMonitor {
    pub(crate) cdp: Arc<dyn CdpConnection>,
    pub(crate) callback: parking_lot::RwLock<Option<ForceRestartCallback>>,
    pub(crate) counters: Arc<MonitorCounters>,
    pub(crate) ping_interval: Duration,
    pub(crate) timeout_threshold: u8,
}

impl ChromiumProcessMonitor {
    pub fn new(cdp: Arc<dyn CdpConnection>) -> Self {
        Self {
            cdp,
            callback: parking_lot::RwLock::new(None),
            counters: MonitorCounters::new(),
            ping_interval: DEFAULT_PING_INTERVAL,
            timeout_threshold: DEFAULT_TIMEOUT_THRESHOLD,
        }
    }

    /// Override the ping cadence. Used by tests.
    pub fn with_ping_interval(mut self, d: Duration) -> Self {
        self.ping_interval = d;
        self
    }

    /// Override the timeout threshold. Used by tests.
    pub fn with_timeout_threshold(mut self, n: u8) -> Self {
        self.timeout_threshold = n;
        self
    }
}

/// Public ProcessMonitor trait.
pub trait ProcessMonitor: Send + Sync {
    /// Register the force-restart callback. Called once at boot by
    /// `Supervisor::start`. Subsequent calls overwrite (last writer
    /// wins) — useful only for test injection.
    fn register_force_restart(&self, callback: ForceRestartCallback);

    /// Spawn the watchdog tokio task. Returns immediately; the task
    /// runs until `shutdown()` is called or the response channel
    /// closes.
    fn spawn(&self) -> Result<(), MonitorError>;

    /// Tell the monitor that a Chromium restart has completed and pings
    /// may resume. Called by `Supervisor::handle_crash` after a
    /// successful restart.
    fn notify_resumed(&self);

    /// Stop the watchdog tokio task.
    fn shutdown(&self);

    /// Snapshot the current counters. Used by observability.
    fn counters(&self) -> Arc<MonitorCounters>;
}

#[derive(Debug, thiserror::Error)]
pub enum MonitorError {
    #[error("force-restart callback not registered")]
    NoCallback,
    #[error("monitor already running")]
    AlreadyRunning,
}

impl ProcessMonitor for ChromiumProcessMonitor {
    fn register_force_restart(&self, callback: ForceRestartCallback) {
        *self.callback.write() = Some(callback);
    }

    fn spawn(&self) -> Result<(), MonitorError> {
        if self.callback.read().is_none() {
            return Err(MonitorError::NoCallback);
        }
        // Phase 6: tokio interval ticker for CDP ping watchdog
        Ok(())
    }

    fn notify_resumed(&self) {
        self.counters
            .consecutive_timeouts
            .store(0, Ordering::Relaxed);
        self.counters.suspended.store(false, Ordering::Relaxed);
    }

    fn shutdown(&self) {
        // Phase 6: signal watchdog task to stop
    }

    fn counters(&self) -> Arc<MonitorCounters> {
        Arc::clone(&self.counters)
    }
}

/// Pure helper: decide whether the latest timeout count crosses the
/// hang threshold. Used by tests + the watchdog body.
pub fn should_trigger_restart(consecutive_timeouts: u8, threshold: u8) -> bool {
    consecutive_timeouts >= threshold
}
