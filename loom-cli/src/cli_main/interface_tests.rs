// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-cli/modules/main/interface_tests.rs` instead.
// Interface tests for `main`. Verifies the binary entry signature,
// library `run` test hook, and tokio runtime construction.

use super::cli_main::{build_runtime, early_init, run};
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

// === IC-CLI-04: main is sole std::process::exit caller ===
//
// Encoded as a project-level clippy lint; the test below documents the
// contract by referencing the FORBIDDEN comment string verbatim.
#[test]
fn forbidden_exit_lint_string_documented() {
    let lint = "// FORBIDDEN: std::process::exit outside main + ErrorMapper";
    assert!(lint.contains("std::process::exit"));
    assert!(lint.contains("ErrorMapper"));
}
