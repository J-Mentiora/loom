//! Performance benchmark harness — session create latency + receipt overhead + binary size.
//! AC-PERF-01.1, AC-PERF-02.1, AC-PERF-04.1.
//!
//! Invoked by `loom benchmark` CLI subcommand. All benchmarks run in-process;
//! no daemon required.

pub mod binary_size;
pub mod harness;
pub mod receipt_overhead;
pub mod session_create;

use serde::{Deserialize, Serialize};

pub use binary_size::BinarySizeReport;
pub use receipt_overhead::ReceiptOverheadReport;
pub use session_create::SessionCreateReport;

/// Top-level configuration for a full benchmark run.
#[derive(Debug, Clone)]
pub struct BenchmarkConfig {
    /// Number of session-create and receipt-overhead iterations (min: 1).
    pub iterations: u32,
    /// Synthetic platform latency per action in ms (default: 5).
    /// Subtracted from measured wall-clock to compute receipt overhead.
    pub t_platform_ms: u64,
    /// Optional path to the loom binary. Required for binary-size check.
    pub binary_path: Option<std::path::PathBuf>,
    /// Override path to meta.json. Defaults to `binary_path/../meta.json`.
    pub meta_json_path: Option<std::path::PathBuf>,
    /// Skip the binary-size check (e.g. in dev builds without meta.json).
    pub skip_binary_size: bool,
}

impl Default for BenchmarkConfig {
    fn default() -> Self {
        Self {
            iterations: 1000,
            t_platform_ms: 5,
            binary_path: None,
            meta_json_path: None,
            skip_binary_size: false,
        }
    }
}

/// Full benchmark report. Serializes to canonical JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReport {
    pub session_create: SessionCreateReport,
    pub receipt_overhead: ReceiptOverheadReport,
    pub binary_size: Option<BinarySizeReport>,
    pub overall_pass: bool,
    pub nfr_kills: Vec<String>,
    /// Non-blocking limitation notes (e.g., excluded overhead sources).
    pub notes: Vec<String>,
}

/// Run all benchmarks and return a report.
///
/// # Errors
/// Returns `BenchmarkError` if configuration is invalid (e.g. iterations=0).
pub fn run_all(config: &BenchmarkConfig) -> Result<BenchmarkReport, BenchmarkError> {
    if config.iterations < 1 {
        return Err(BenchmarkError::InvalidIterations);
    }

    let sc = session_create::run(config)?;
    let ro = receipt_overhead::run(config)?;

    let bs = if config.skip_binary_size {
        None
    } else {
        let binary_path = config
            .binary_path
            .clone()
            .ok_or(BenchmarkError::BinaryPathRequired)?;
        // AC-BENCH-02: when --meta-json is NOT given, compute the default
        // sibling path and fall back to stat-only if it's absent.
        // When --meta-json IS given explicitly, honour the user's intent:
        // hard-fail if the file is absent (AC-BENCH-04).
        let report = if let Some(explicit_meta) = config.meta_json_path.clone() {
            binary_size::run(&binary_path, &explicit_meta)?
        } else {
            let default_meta = binary_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join("meta.json");
            if default_meta.exists() {
                binary_size::run(&binary_path, &default_meta)?
            } else {
                binary_size::run_stat_fallback(&binary_path)?
            }
        };
        Some(report)
    };

    let mut nfr_kills = Vec::new();
    if sc.nfr_kill {
        nfr_kills.push(format!(
            "NFR-KILL: session_create p99 {}ms > 1000ms",
            sc.p99_ms
        ));
    }
    if ro.nfr_kill {
        nfr_kills.push(format!(
            "NFR-KILL: receipt_overhead p95 {}ms > 200ms",
            ro.p95_overhead_ms
        ));
    }

    let binary_pass = bs.as_ref().map(|b| b.pass).unwrap_or(true);
    let overall_pass = sc.pass && ro.pass && binary_pass && nfr_kills.is_empty();

    Ok(BenchmarkReport {
        session_create: sc,
        receipt_overhead: ro,
        binary_size: bs,
        overall_pass,
        nfr_kills,
        notes: vec![
            "WIT/WASM marshalling overhead excluded; requires loom-host integration for full stack measurement.".to_string(),
        ],
    })
}

/// Errors from the benchmark harness.
#[derive(Debug)]
pub enum BenchmarkError {
    /// iterations must be >= 1.
    InvalidIterations,
    /// binary_path is required when skip_binary_size is false.
    BinaryPathRequired,
    /// meta.json file not found at the expected path.
    MetaJsonNotFound(std::path::PathBuf),
    /// meta.json could not be parsed.
    MetaJsonParseError(String),
    /// Binary file not accessible.
    BinaryNotFound(std::path::PathBuf),
    /// I/O error building the mock session stack.
    SetupError(String),
}

impl std::fmt::Display for BenchmarkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidIterations => write!(f, "iterations must be >= 1"),
            Self::BinaryPathRequired => {
                write!(f, "binary_path required when skip_binary_size is false")
            }
            Self::MetaJsonNotFound(p) => write!(f, "meta.json not found: {}", p.display()),
            Self::MetaJsonParseError(e) => write!(f, "meta.json parse error: {e}"),
            Self::BinaryNotFound(p) => write!(f, "binary not found: {}", p.display()),
            Self::SetupError(e) => write!(f, "benchmark setup error: {e}"),
        }
    }
}
