// Interface tests for BenchmarkArgs and run_benchmark contract.
// AC-PERF-01.1, AC-PERF-02.1, AC-PERF-04.1 (CLI surface tests).

use super::benchmark_commands::{run_benchmark, BenchmarkArgs};
use crate::cli_config::CliConfig;
use crate::CliError;

fn default_args() -> BenchmarkArgs {
    BenchmarkArgs {
        iterations: 5,
        t_platform_ms: 5,
        binary: None,
        meta_json: None,
        skip_binary_size: true,
    }
}

fn default_config() -> CliConfig {
    crate::cli_config::compiled_defaults()
}

/// AC: exit 0 when all SLAs pass (skip binary size check, fast mock).
#[test]
fn test_benchmark_command_exit_0_on_pass() {
    let args = default_args();
    let result = run_benchmark(&args, &default_config());
    assert!(result.is_ok(), "expected Ok(()), got {:?}", result);
}

/// AC: exit 1 (CliError::Receipt) when iterations=0 causes BenchmarkError.
#[test]
fn test_benchmark_command_exit_1_on_usage_zero_iterations() {
    let mut args = default_args();
    args.iterations = 0;
    let result = run_benchmark(&args, &default_config());
    assert!(
        matches!(result, Err(CliError::Usage(_))),
        "expected Usage error for iterations=0, got {:?}",
        result
    );
}

/// AC: --skip-binary-size skips the binary size check even without --binary.
#[test]
fn test_benchmark_command_skip_binary_size() {
    let mut args = default_args();
    args.skip_binary_size = true;
    args.binary = None;
    let result = run_benchmark(&args, &default_config());
    assert!(result.is_ok(), "skip_binary_size should not fail without binary: {:?}", result);
}

/// AC: --binary required error when skip_binary_size=false and no --binary.
#[test]
fn test_benchmark_command_binary_required_error() {
    let mut args = default_args();
    args.skip_binary_size = false;
    args.binary = None;
    let result = run_benchmark(&args, &default_config());
    assert!(
        matches!(result, Err(CliError::Usage(_))),
        "expected Usage error when binary path missing, got {:?}",
        result
    );
}

/// AC-BENCH-02: --binary given without meta.json → uses stat fallback, no error.
#[test]
fn test_benchmark_with_binary_no_meta_json() {
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    // Create a small binary file (no sibling meta.json).
    let bin_path = dir.path().join("loom");
    let f = std::fs::File::create(&bin_path).unwrap();
    f.set_len(10 * 1024 * 1024).unwrap(); // 10 MB < 60 MB conservative budget

    let mut args = default_args();
    args.skip_binary_size = false;
    args.binary = Some(bin_path);
    args.meta_json = None;

    let result = run_benchmark(&args, &default_config());
    assert!(
        result.is_ok(),
        "AC-BENCH-02: --binary given without meta.json should succeed via stat fallback, got {:?}",
        result
    );
}

/// AC-BENCH-04: explicit --meta-json pointing at missing file → Usage error with remediation.
#[test]
fn test_benchmark_explicit_meta_json_missing() {
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let bin_path = dir.path().join("loom");
    std::fs::File::create(&bin_path).unwrap().set_len(5 * 1024 * 1024).unwrap();
    let missing_meta = dir.path().join("missing.json");

    let mut args = default_args();
    args.skip_binary_size = false;
    args.binary = Some(bin_path);
    args.meta_json = Some(missing_meta);

    let result = run_benchmark(&args, &default_config());
    match &result {
        Err(CliError::Usage(msg)) => {
            assert!(
                msg.contains("just gen-meta") || msg.contains("meta.json not found"),
                "AC-BENCH-04: remediation message should mention gen-meta, got: {msg}"
            );
        }
        other => panic!("AC-BENCH-04: expected Usage error with remediation, got {:?}", other),
    }
}
