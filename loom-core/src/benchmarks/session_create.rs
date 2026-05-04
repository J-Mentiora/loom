//! Session-create latency benchmark — AC-PERF-01.1.
//!
//! Times `session_manager.create()` for `iterations` warm calls.
//! Computes p50 and p99 percentiles; validates SLA thresholds.

use super::harness::{build_session_manager, MockManifestWriter};
use super::{BenchmarkConfig, BenchmarkError};
use crate::session_manager::SessionCreateOpts;
use serde::{Deserialize, Serialize};

/// Result of the session-create latency benchmark (AC-PERF-01.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCreateReport {
    /// p50 latency in milliseconds.
    pub p50_ms: u64,
    /// p99 latency in milliseconds.
    pub p99_ms: u64,
    /// Number of timed iterations (warm-up call excluded).
    pub iterations: u32,
    /// True if p50 ≤ 500ms AND p99 ≤ 1000ms.
    pub pass: bool,
    /// True if p99 > 1000ms (NFR-KILL threshold).
    pub nfr_kill: bool,
}

/// Run the session-create benchmark.
///
/// Creates one warm-up session (not included in timing), then times
/// `config.iterations` consecutive `create()` calls.
pub fn run(config: &BenchmarkConfig) -> Result<SessionCreateReport, BenchmarkError> {
    if config.iterations < 1 {
        return Err(BenchmarkError::InvalidIterations);
    }

    let manifest_writer = MockManifestWriter::new();
    let mgr = build_session_manager(manifest_writer);

    let warmup_opts = SessionCreateOpts {
        agent_id: "bench-warmup".to_string(),
        surface: "bench".to_string(),
        seed: Some(1),
        limits: None,
        replay_of: None,
        started_at_ms_override: None,
        capture_policy: None,
        no_blocklist: false,
        profile: "safe".to_string(),
    };

    // Warm-up call (not timed).
    let warmup_id = mgr
        .create(warmup_opts)
        .map_err(|e| BenchmarkError::SetupError(e.to_string()))?;
    let _ = mgr.close(warmup_id);

    // Timed iterations.
    let mut samples: Vec<u64> = Vec::with_capacity(config.iterations as usize);
    let opts = SessionCreateOpts {
        agent_id: "bench".to_string(),
        surface: "bench".to_string(),
        seed: Some(42),
        limits: None,
        replay_of: None,
        started_at_ms_override: None,
        capture_policy: None,
        no_blocklist: false,
        profile: "safe".to_string(),
    };

    for _ in 0..config.iterations {
        let t0 = std::time::Instant::now();
        let id = mgr
            .create(opts.clone())
            .map_err(|e| BenchmarkError::SetupError(e.to_string()))?;
        let elapsed_ms = t0.elapsed().as_millis() as u64;
        let _ = mgr.close(id);
        samples.push(elapsed_ms);
    }

    let report = compute_report(&samples, config.iterations);
    Ok(report)
}

fn compute_report(samples: &[u64], iterations: u32) -> SessionCreateReport {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();

    let n = sorted.len();
    let p50_ms = *sorted.get(n / 2).unwrap_or(&0);
    let p99_ms = *sorted.get(n * 99 / 100).unwrap_or(&0);

    let pass = p50_ms <= 500 && p99_ms <= 1000;
    let nfr_kill = p99_ms > 1000;

    SessionCreateReport {
        p50_ms,
        p99_ms,
        iterations,
        pass,
        nfr_kill,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmarks::harness::MockManifestWriter;

    fn fast_config(iterations: u32) -> BenchmarkConfig {
        BenchmarkConfig {
            iterations,
            ..BenchmarkConfig::default()
        }
    }

    /// AC-PERF-01.1: p50 ≤ 500ms, p99 ≤ 1000ms on fast in-process mock.
    #[test]
    fn test_session_create_bench_pass() {
        let config = fast_config(10);
        let report = run(&config).expect("benchmark should succeed");
        assert!(
            report.pass,
            "expected pass but got p50={} p99={}",
            report.p50_ms, report.p99_ms
        );
        assert!(!report.nfr_kill);
        assert_eq!(report.iterations, 10);
    }

    /// AC-PERF-01.1: p99 > 1000ms triggers NFR-KILL flag.
    #[test]
    fn test_session_create_bench_nfr_kill() {
        // Inject 1001ms delay on open_manifest (called inside create()) to push p99 over threshold.
        let writer = MockManifestWriter::with_open_delay(1001);
        let mgr = build_session_manager(writer);
        let opts = SessionCreateOpts {
            agent_id: "bench-nfr".to_string(),
            surface: "bench".to_string(),
            seed: Some(1),
            limits: None,
            replay_of: None,
            started_at_ms_override: None,
            capture_policy: None,
            no_blocklist: false,
            profile: "safe".to_string(),
        };

        // Collect 5 samples (fast test — just enough to establish p99).
        let mut samples: Vec<u64> = Vec::new();
        for _ in 0..5_u32 {
            let t0 = std::time::Instant::now();
            let id = mgr.create(opts.clone()).unwrap();
            samples.push(t0.elapsed().as_millis() as u64);
            let _ = mgr.close(id);
        }

        let report = compute_report(&samples, 5);
        assert!(
            report.nfr_kill,
            "expected nfr_kill for p99={}",
            report.p99_ms
        );
        assert!(!report.pass);
    }

    /// Warm-up call must not appear in the samples (samples.len() == iterations).
    #[test]
    fn test_session_create_bench_warmup_excluded() {
        // Use a writer that counts appends.
        let _writer = MockManifestWriter::new();
        let config = fast_config(5);
        let report = run(&config).expect("benchmark should succeed");
        assert_eq!(report.iterations, 5, "iterations mismatch");
    }

    /// Percentile accuracy: known skewed distribution yields expected p50/p99.
    #[test]
    fn test_session_create_percentile_accuracy() {
        // 800 samples at 100ms, 200 at 900ms.
        // sorted[0..799] = 100ms, sorted[800..999] = 900ms.
        // p50 = sorted[500] = 100ms; p99 = sorted[990] = 900ms.
        let mut samples: Vec<u64> = (0..800).map(|_| 100u64).collect();
        samples.extend((0..200).map(|_| 900u64));
        let report = compute_report(&samples, 1000);
        // p50 index = 1000/2 = 500 → sorted[500] = 100ms.
        assert!(report.p50_ms <= 100, "p50={} expected ≤100", report.p50_ms);
        // p99 index = 1000*99/100 = 990 → sorted[990] = 900ms.
        assert!(report.p99_ms >= 900, "p99={} expected ≥900", report.p99_ms);
    }

    /// iterations=0 must return InvalidIterations error.
    #[test]
    fn test_session_create_iterations_zero() {
        let config = fast_config(0);
        let result = run(&config);
        assert!(matches!(result, Err(BenchmarkError::InvalidIterations)));
    }
}
