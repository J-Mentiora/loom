// Benchmark runner implementation.
// Builds BenchmarkConfig from CLI args, runs loom_core::benchmarks::run_all,
// outputs result, maps errors to CliError.

use loom_core::benchmarks::{BenchmarkConfig, BenchmarkError};

use crate::benchmark_commands::BenchmarkArgs;
use crate::cli_config::CliConfig;
use crate::output_formatter::emit_to_stdout;
use crate::CliError;

pub fn run(args: &BenchmarkArgs, cfg: &CliConfig) -> Result<(), CliError> {
    // Validate iterations.
    if args.iterations < 1 {
        return Err(CliError::Usage("--iterations must be >= 1".to_string()));
    }

    let config = BenchmarkConfig {
        iterations: args.iterations,
        t_platform_ms: args.t_platform_ms,
        binary_path: args.binary.clone(),
        meta_json_path: args.meta_json.clone(),
        skip_binary_size: args.skip_binary_size,
    };

    let report = loom_core::benchmarks::run_all(&config).map_err(|e| match e {
        BenchmarkError::InvalidIterations => {
            CliError::Usage("--iterations must be >= 1".to_string())
        }
        BenchmarkError::BinaryPathRequired => CliError::Usage(
            "--binary path required when --skip-binary-size is not set. \
             Pass --skip-binary-size to run latency benchmarks without the binary-size check."
                .to_string(),
        ),
        // AC-BENCH-04: clear remediation when meta.json missing.
        // Note: with the stat-fallback in run_all(), this arm fires only when
        // --meta-json is set explicitly but the file is absent.
        BenchmarkError::MetaJsonNotFound(p) => CliError::Usage(format!(
            "meta.json not found at {}. \
             Run `just gen-meta` in src/ to generate it, \
             or pass --skip-binary-size to run latency benchmarks only.",
            p.display()
        )),
        other => CliError::Internal(other.to_string()),
    })?;

    let value = serde_json::to_value(&report).map_err(|e| CliError::Internal(e.to_string()))?;
    emit_to_stdout("benchmark", &value, cfg, None)?;

    // Return error if SLAs failed (exit 1).
    if !report.overall_pass {
        let receipt = serde_json::json!({
            "status": "error",
            "code": "BENCHMARK_SLA_FAILED",
            "nfr_kills": report.nfr_kills,
            "message": format!("Performance SLA failed: {}", report.nfr_kills.join("; "))
        });
        return Err(CliError::Receipt(receipt));
    }

    Ok(())
}
