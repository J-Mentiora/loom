// Interface tests for `LogForwarder`.
// Verifies fail-open semantics, critical-line classification.

use super::log_forwarder::{classify_line, ForwarderCounters, LogLevel};
use std::sync::atomic::Ordering;
use std::sync::Arc;

// === Fail-open: drop counters present + atomic ===

#[test]
fn forwarder_counters_are_atomic_and_default_zero() {
    let c: Arc<ForwarderCounters> = ForwarderCounters::new();
    assert_eq!(c.lines_forwarded.load(Ordering::Relaxed), 0);
    assert_eq!(c.lines_dropped_channel_full.load(Ordering::Relaxed), 0);
    assert_eq!(c.lines_dropped_pipe_broken.load(Ordering::Relaxed), 0);
}

#[test]
fn forwarder_counters_no_floats() {
    let c = ForwarderCounters::new();
    let _: u64 = c.lines_forwarded.load(Ordering::Relaxed);
}

// === Classification: critical lines ===

#[test]
fn fatal_line_is_critical_error() {
    let (level, is_crit) = classify_line("FATAL: too bad to continue");
    assert_eq!(level, LogLevel::Error);
    assert!(is_crit);
}

#[test]
fn check_failed_line_is_critical_error() {
    let (level, is_crit) = classify_line("[12345:0] CHECK failed: condition");
    assert_eq!(level, LogLevel::Error);
    assert!(is_crit);
}

#[test]
fn chromium_crashed_line_is_critical_error() {
    let (level, is_crit) = classify_line("hint: chromium crashed during paint");
    assert_eq!(level, LogLevel::Error);
    assert!(is_crit);
}

// === Classification: non-critical levels ===

#[test]
fn warn_line_is_warn_not_critical() {
    let (level, is_crit) = classify_line("WARN: cookie store slow");
    assert_eq!(level, LogLevel::Warn);
    assert!(!is_crit);
}

#[test]
fn info_line_default_info() {
    let (level, is_crit) = classify_line("session started");
    assert_eq!(level, LogLevel::Info);
    assert!(!is_crit);
}

#[test]
fn debug_line_is_debug() {
    let (level, is_crit) = classify_line("verbose: scheduler tick");
    assert_eq!(level, LogLevel::Debug);
    assert!(!is_crit);
}

#[test]
fn error_line_without_fatal_prefix_is_error_not_critical() {
    let (level, is_crit) = classify_line("ERROR: bad happened");
    assert_eq!(level, LogLevel::Error);
    assert!(!is_crit);
}

// === LogLevel string mapping (consumed by ShimResponse::LogLine.level) ===

#[test]
fn log_level_strings_match_tracing_convention() {
    assert_eq!(LogLevel::Trace.as_str(), "trace");
    assert_eq!(LogLevel::Debug.as_str(), "debug");
    assert_eq!(LogLevel::Info.as_str(), "info");
    assert_eq!(LogLevel::Warn.as_str(), "warn");
    assert_eq!(LogLevel::Error.as_str(), "error");
}
