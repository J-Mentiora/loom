//! Receipt-generation overhead benchmark.
//!
//! Times the receipt_builder + manifest_append pipeline for a synthetic
//! 10-action corpus × `iterations` rounds.
//!
//! Overhead = wall_clock_per_action - config.t_platform_ms (injected synthetic
//! CDP latency). p95 must be ≤ 50ms; NFR-KILL at > 200ms.
//!
//! Limitation: WIT/WASM marshalling overhead is excluded. Requires
//! loom-host integration testing for full stack measurement.

use super::harness::{build_session_manager, MockManifestWriter};
use super::{BenchmarkConfig, BenchmarkError};
use crate::manifest_writer::{ManifestEntry, ManifestWriter};
use crate::receipt_builder::ReceiptBuilder;
use crate::session_manager::SessionCreateOpts;
use serde::{Deserialize, Serialize};

/// Number of synthetic actions in the benchmark corpus.
const CORPUS_SIZE: u32 = 10;

/// Result of the receipt-overhead benchmark.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiptOverheadReport {
    /// p95 overhead (wall_clock - t_platform) per action in milliseconds.
    pub p95_overhead_ms: u64,
    /// Number of timing samples collected (corpus_size × iterations).
    pub sample_count: u64,
    /// Number of iterations of the full corpus.
    pub iterations: u32,
    /// True if p95_overhead_ms ≤ 50ms.
    pub pass: bool,
    /// True if p95_overhead_ms > 200ms (NFR-KILL threshold).
    pub nfr_kill: bool,
    pub notes: Vec<String>,
}

/// Run the receipt-overhead benchmark.
pub fn run(config: &BenchmarkConfig) -> Result<ReceiptOverheadReport, BenchmarkError> {
    if config.iterations < 1 {
        return Err(BenchmarkError::InvalidIterations);
    }

    let manifest_writer = MockManifestWriter::new();
    let mgr = build_session_manager(manifest_writer.clone());

    let session_opts = SessionCreateOpts {
        agent_id: "bench-overhead".to_string(),
        surface: "bench".to_string(),
        seed: Some(42),
        limits: None,
        replay_of: None,
        started_at_ms_override: None,
        capture_policy: None,
        no_blocklist: false,
        no_determinism: false,
        record_screencast: false,
        audio: false,
        profile: "safe".to_string(),
    };

    let session_id = mgr
        .create(session_opts)
        .map_err(|e| BenchmarkError::SetupError(e.to_string()))?;

    let mut samples: Vec<u64> = Vec::with_capacity((config.iterations * CORPUS_SIZE) as usize);

    for iter in 0..config.iterations {
        for action_idx in 0..CORPUS_SIZE {
            let action_id = (iter * CORPUS_SIZE + action_idx) as u64;

            let t0 = std::time::Instant::now();

            // Build synthetic click receipt (no I/O).
            let receipt = ReceiptBuilder::build_click_receipt(
                action_id.to_string(),
                /* timing_ticks */ config.t_platform_ms * 1_000_000,
                format!("sha256-dom-{action_id:064x}"),
                format!("sha256-ss-{action_id:064x}"),
            );

            // Canonicalize the receipt (JCS via serde_jcs).
            let canonical_bytes = serde_jcs::to_vec(&receipt)
                .unwrap_or_else(|_| serde_json::to_vec(&receipt).unwrap_or_default());

            // Append to manifest (the I/O that contributes to overhead).
            let prev_hash = format!("{action_id:064x}");
            manifest_writer
                .append(
                    session_id.clone(),
                    ManifestEntry::ActionReceipt {
                        action_id,
                        emitted_at_ms: 0,
                        receipt_canonical_bytes: canonical_bytes,
                        prev_hash,
                    },
                )
                .map_err(|e| BenchmarkError::SetupError(e.to_string()))?;

            let elapsed_ms = t0.elapsed().as_millis() as u64;
            let overhead_ms = elapsed_ms.saturating_sub(config.t_platform_ms);
            samples.push(overhead_ms);
        }
    }

    let _ = mgr.close(session_id);

    let report = compute_report(&samples, config.iterations);
    Ok(report)
}

fn compute_report(samples: &[u64], iterations: u32) -> ReceiptOverheadReport {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();

    let n = sorted.len();
    let p95_overhead_ms = *sorted.get(n * 95 / 100).unwrap_or(&0);

    let pass = p95_overhead_ms <= 50;
    let nfr_kill = p95_overhead_ms > 200;

    ReceiptOverheadReport {
        p95_overhead_ms,
        sample_count: samples.len() as u64,
        iterations,
        pass,
        nfr_kill,
        notes: vec![
            "WIT/WASM marshalling overhead excluded; loom-host integration required for full stack."
                .to_string(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast_config(iterations: u32) -> BenchmarkConfig {
        BenchmarkConfig {
            iterations,
            t_platform_ms: 5,
            ..BenchmarkConfig::default()
        }
    }

    /// p95 ≤ 50ms on fast in-process mock.
    #[test]
    fn test_receipt_overhead_bench_pass() {
        let config = fast_config(5);
        let report = run(&config).expect("benchmark should succeed");
        assert!(
            report.pass,
            "expected pass but got p95={}ms",
            report.p95_overhead_ms
        );
        assert!(!report.nfr_kill);
    }

    /// p95 > 200ms triggers NFR-KILL.
    #[test]
    fn test_receipt_overhead_bench_nfr_kill() {
        // Inject 250ms delay per manifest append to push overhead over NFR-KILL.
        let writer = MockManifestWriter::with_append_delay(250);
        let mgr = build_session_manager(writer.clone());

        let session_id = mgr
            .create(SessionCreateOpts {
                agent_id: "nfr-kill-test".to_string(),
                surface: "bench".to_string(),
                seed: Some(1),
                limits: None,
                replay_of: None,
                started_at_ms_override: None,
                capture_policy: None,
                no_blocklist: false,
                no_determinism: false,
                record_screencast: false,
                audio: false,
                profile: "safe".to_string(),
            })
            .unwrap();

        let t_platform_ms = 5u64;
        let mut samples: Vec<u64> = Vec::new();
        for i in 0..10_u64 {
            let t0 = std::time::Instant::now();
            let receipt = ReceiptBuilder::build_click_receipt(
                i.to_string(),
                t_platform_ms * 1_000_000,
                format!("{i:064x}"),
                format!("{i:064x}"),
            );
            let canonical = serde_jcs::to_vec(&receipt)
                .unwrap_or_else(|_| serde_json::to_vec(&receipt).unwrap_or_default());
            writer
                .append(
                    session_id.clone(),
                    ManifestEntry::ActionReceipt {
                        action_id: i,
                        emitted_at_ms: 0,
                        receipt_canonical_bytes: canonical,
                        prev_hash: format!("{i:064x}"),
                    },
                )
                .unwrap();
            let elapsed = t0.elapsed().as_millis() as u64;
            samples.push(elapsed.saturating_sub(t_platform_ms));
        }

        let _ = mgr.close(session_id);
        let report = compute_report(&samples, 1);
        assert!(
            report.nfr_kill,
            "expected nfr_kill for p95={}ms",
            report.p95_overhead_ms
        );
        assert!(!report.pass);
    }

    /// Saturating subtraction: if elapsed < t_platform, overhead = 0 (no negatives).
    #[test]
    fn test_receipt_overhead_saturation_subtraction() {
        // 5ms elapsed, 10ms t_platform → saturating_sub → 0.
        let samples = [5u64];
        let t_platform = 10u64;
        let overhead: Vec<u64> = samples
            .iter()
            .map(|&e| e.saturating_sub(t_platform))
            .collect();
        assert_eq!(overhead[0], 0, "expected 0 from saturating_sub(5, 10)");
    }

    /// Sample count = CORPUS_SIZE × iterations.
    #[test]
    fn test_receipt_overhead_sample_count() {
        let config = fast_config(10);
        let report = run(&config).expect("benchmark should succeed");
        let expected = (CORPUS_SIZE * 10) as u64;
        assert_eq!(
            report.sample_count, expected,
            "expected {} samples, got {}",
            expected, report.sample_count
        );
    }

    /// Percentile formula: p95 = sorted[n * 95 / 100].
    #[test]
    fn test_receipt_overhead_p95_formula() {
        // Inject known distribution: 95 samples at 10ms, 5 at 300ms.
        let mut samples: Vec<u64> = (0..95).map(|_| 10u64).collect();
        samples.extend((0..5).map(|_| 300u64));
        let report = compute_report(&samples, 1);
        // p95 index = 100 * 95 / 100 = 95 → sorted[95] = 300.
        assert_eq!(
            report.p95_overhead_ms, 300,
            "p95 should be 300ms at index 95"
        );
        assert!(report.nfr_kill, "300ms > 200ms should be NFR-KILL");
    }

    /// iterations=0 returns InvalidIterations.
    #[test]
    fn test_receipt_overhead_iterations_zero() {
        let config = BenchmarkConfig {
            iterations: 0,
            ..BenchmarkConfig::default()
        };
        assert!(matches!(
            run(&config),
            Err(BenchmarkError::InvalidIterations)
        ));
    }
}
