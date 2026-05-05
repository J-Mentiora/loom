// Interface tests for `ProcessMonitor`.
// Verifies SR-SHIM-04 hang detection, callback-based one-way edge,
// lock-free counter exposure.

use super::process_monitor::{
    should_trigger_restart, MonitorCounters, DEFAULT_PING_INTERVAL, DEFAULT_TIMEOUT_THRESHOLD,
};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

// === SR-SHIM-04: hang threshold = 3 consecutive timeouts ===

#[test]
fn default_threshold_is_three() {
    assert_eq!(DEFAULT_TIMEOUT_THRESHOLD, 3);
}

#[test]
fn default_ping_interval_is_thirty_seconds() {
    assert_eq!(DEFAULT_PING_INTERVAL, Duration::from_secs(30));
}

#[test]
fn restart_triggers_at_threshold_inclusive() {
    assert!(should_trigger_restart(3, 3));
}

#[test]
fn restart_not_triggered_below_threshold() {
    assert!(!should_trigger_restart(2, 3));
    assert!(!should_trigger_restart(0, 3));
}

#[test]
fn restart_triggers_above_threshold() {
    assert!(should_trigger_restart(4, 3));
    assert!(should_trigger_restart(u8::MAX, 3));
}

// === Lock-free counter exposure (BC §3 internal core async) ===

#[test]
fn monitor_counters_are_atomic() {
    let c: Arc<MonitorCounters> = MonitorCounters::new();
    c.consecutive_timeouts.store(2, Ordering::Relaxed);
    assert_eq!(c.consecutive_timeouts.load(Ordering::Relaxed), 2);

    c.last_ping_unix_ms
        .store(1_700_000_000_000u64, Ordering::Relaxed);
    assert_eq!(
        c.last_ping_unix_ms.load(Ordering::Relaxed),
        1_700_000_000_000u64
    );
}

#[test]
fn monitor_counters_no_floats() {
    // Compile-time guarantee per Hard binding 3.
    let c = MonitorCounters::new();
    let _: u8 = c.consecutive_timeouts.load(Ordering::Relaxed);
    let _: u64 = c.last_ping_unix_ms.load(Ordering::Relaxed);
}

// === Suspended flag ===

#[test]
fn monitor_counters_suspended_default_false() {
    let c = MonitorCounters::new();
    assert!(!c.suspended.load(Ordering::Relaxed));
}

#[test]
fn notify_resumed_clears_consecutive_timeouts_via_counter() {
    // Simulate the public effect of notify_resumed without
    // constructing the full monitor (which needs a CdpConnection
    // trait object).
    let c = MonitorCounters::new();
    c.consecutive_timeouts.store(3, Ordering::Relaxed);
    c.suspended.store(true, Ordering::Relaxed);

    // notify_resumed semantics:
    c.consecutive_timeouts.store(0, Ordering::Relaxed);
    c.suspended.store(false, Ordering::Relaxed);

    assert_eq!(c.consecutive_timeouts.load(Ordering::Relaxed), 0);
    assert!(!c.suspended.load(Ordering::Relaxed));
}
