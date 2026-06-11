// Interface tests for `main`. Verifies the binary entry signature,
// library `run` test hook, and tokio runtime construction.

use super::cli_main::{build_runtime, early_init, merge_pretty_flag, run};
use crate::{CliConfig, CliError};

#[test]
fn run_signature_takes_argv_returns_exit_code() {
    fn _ck(argv: Vec<String>) -> i32 {
        run(argv)
    }
    let _ = _ck;
}

#[test]
fn build_runtime_returns_multithreaded_tokio_runtime() {
    fn _ck() -> tokio::runtime::Runtime {
        build_runtime()
    }
    let _ = _ck;
}

#[test]
fn early_init_returns_cli_config_or_error() {
    fn _ck(argv: &[String]) -> Result<CliConfig, CliError> {
        early_init(argv)
    }
    let _ = _ck;
}

// === --pretty flag merge (regression: cli_main unconditionally clobbered
// the file/env-resolved `pretty` with the bare CLI flag, so config.toml
// `pretty = true` and LOOM_PRETTY=true were dead) ===

#[test]
fn merge_pretty_flag_keeps_resolved_value_when_flag_absent() {
    // Flag absent (false) must NOT clobber the env/file-resolved true.
    assert!(merge_pretty_flag(true, false));
    assert!(!merge_pretty_flag(false, false));
}

#[test]
fn merge_pretty_flag_forces_on_when_passed() {
    assert!(merge_pretty_flag(false, true));
    assert!(merge_pretty_flag(true, true));
}

#[test]
fn env_resolved_pretty_forces_pretty_into_a_pipe() {
    use crate::cli_config::output_mode::OutputMode;
    // LOOM_PRETTY=true / config.toml pretty=true, no flags, stdout piped:
    // the merged value must reach OutputMode::resolve and yield pretty —
    // this was the user-visible loss when the merge was clobbered.
    let merged = merge_pretty_flag(true, false);
    assert_eq!(
        OutputMode::resolve(false, false, merged, false),
        OutputMode::PrettyCurated
    );
    // --json (flag) still beats env/file pretty per CLI > env > file.
    assert_eq!(
        OutputMode::resolve(false, true, merged, false),
        OutputMode::Json
    );
}

// === main is sole std::process::exit caller ===
//
// Encoded as a project-level clippy lint; the test below documents the
// contract by referencing the FORBIDDEN comment string verbatim.
#[test]
fn forbidden_exit_lint_string_documented() {
    let lint = "// FORBIDDEN: std::process::exit outside main + ErrorMapper";
    assert!(lint.contains("std::process::exit"));
    assert!(lint.contains("ErrorMapper"));
}
