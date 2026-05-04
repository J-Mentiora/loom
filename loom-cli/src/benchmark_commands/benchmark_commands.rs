// BenchmarkCommands — handler for `loom benchmark` subcommand.
//
// # Contract semantics
// - Runs the loom-core benchmark harness in-process (no daemon required).
// - Outputs BenchmarkReport as canonical JSON via OutputFormatter.
// - Exit 0 on all SLAs pass; exit 1 if any NFR-KILL or SLA failure.
// - Exit 2 on usage error (--iterations < 1).

use clap::Args;

use crate::cli_config::CliConfig;
use crate::CliError;

/// Arguments for `loom benchmark`.
#[derive(Debug, Clone, Args)]
pub struct BenchmarkArgs {
    /// Number of session-create and receipt-overhead iterations (min: 1).
    #[arg(long, default_value = "1000")]
    pub iterations: u32,

    /// Synthetic platform latency per action in ms for overhead measurement.
    /// This represents the expected CDP call latency, subtracted from
    /// total measured time to compute receipt-generation overhead.
    #[arg(long, default_value = "5")]
    pub t_platform_ms: u64,

    /// Path to the loom binary for binary-size check (defaults to argv[0]).
    #[arg(long)]
    pub binary: Option<std::path::PathBuf>,

    /// Override path to meta.json for binary-size check.
    #[arg(long)]
    pub meta_json: Option<std::path::PathBuf>,

    /// Skip the binary-size check (useful in dev builds without meta.json).
    #[arg(long)]
    pub skip_binary_size: bool,
}

/// Run the benchmark suite and write results to stdout as canonical JSON.
///
/// Returns `Err(CliError::Usage)` on invalid arguments (exit 2),
/// `Err(CliError::Receipt)` if SLAs fail (exit 1), or `Ok(())` on pass (exit 0).
pub fn run_benchmark(args: &BenchmarkArgs, config: &CliConfig) -> Result<(), CliError> {
    crate::benchmark_commands::impl_benchmark::run(args, config.pretty)
}
