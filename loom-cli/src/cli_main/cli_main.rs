// Re-export of the locked Phase 5.3 interface. DO NOT EDIT here.
// Edit `systems/loom-cli/modules/main/interfaces.rs` instead.
// `main` — `loom` binary entrypoint.
//
// # Contract semantics
// - **Entry layer.** Builds the clap parser, initialises the tokio
//   runtime, resolves config, initialises observability, then hands
//   off to `CommandRouter`.
// - **IC-CLI-04 (exit codes).** `main` is the SOLE caller of
//   `std::process::exit`. All exits are mapped through
//   `ErrorMapper::map_exit_code`. The clippy lint
//   `// FORBIDDEN: std::process::exit outside main + ErrorMapper`
//   enforces this elsewhere in the crate.
// - **BC-CLI-02 (config precedence).** `main` invokes
//   `ConfigResolver::resolve(args, env, file)` before constructing
//   `CommandRouter`. The resolved `CliConfig` is owned by `main` and
//   borrowed by handlers via `CommandRouter`.
// - **Tokio runtime.** A single multi-threaded `tokio` runtime is
//   constructed in `main` and dropped at exit. No nested runtimes.
//
// # module_kind
// Binary entry — exports `fn main()` and the `loom-cli` crate's
// library-style `run` helper for integration tests.

use clap::Parser as _;

use crate::cli_config::ResolveInputs;
use crate::command_router::{Cli, dispatch};
use crate::error_mapper::map_exit_code;
use crate::CliConfig;
use crate::CliError;

/// Process entrypoint. Parses argv, builds the runtime, dispatches to
/// `CommandRouter`, and exits with the mapped exit code from
/// `ErrorMapper`.
///
/// IC-CLI-04: SOLE caller of `std::process::exit` in the crate
/// (other than `ErrorMapper::map_exit_code` which supplies the code).
/// // FORBIDDEN: std::process::exit outside main + ErrorMapper
pub fn main() {
    std::process::exit(run(std::env::args().collect()));
}

/// Library-style entrypoint used by integration tests. Identical
/// behaviour to `main()` but returns the exit code instead of calling
/// `std::process::exit`. The real `main()` is a one-line wrapper that
/// passes through to this function.
pub fn run(argv: Vec<String>) -> i32 {
    let rt = build_runtime();
    rt.block_on(async_run(argv))
}

/// Async inner body of `run`. Separated to keep `run` synchronous
/// (required by the `tokio::runtime::Runtime::block_on` API).
async fn async_run(argv: Vec<String>) -> i32 {
    // Parse first — so --help / --version never trigger config I/O
    // (SR-CLI-01: --version p99 ≤ 200 ms, must not read filesystem).
    // Use try_parse_from so the test harness can call run() without
    // triggering std::process::exit.
    let cli = match Cli::try_parse_from(argv.iter().map(String::as_str)) {
        Ok(cli) => cli,
        Err(e) => {
            // exit_code 0 → DisplayHelp / DisplayVersion (print to stdout/stderr)
            // exit_code 2 → usage / parse error
            let code = e.exit_code();
            let _ = e.print(); // prints to the appropriate stream
            return code;
        }
    };

    // BC-CLI-02: ConfigResolver::resolve called before CommandRouter.
    let config = match early_init(&argv) {
        Ok(c) => c,
        Err(e) => {
            // AC-AESF-05: print_error before exit so early-init failures are visible.
            let r = Err(e);
            crate::error_mapper::print_error(&r);
            return map_exit_code(&r);
        }
    };

    // AC-CLIOUT2-03: propagate the global --pretty flag from clap into
    // the resolved config so handlers can route through `format_output`
    // without re-parsing argv. This is the single point where the flag
    // is mirrored; everything downstream reads `cfg.pretty`.
    let mut config = config;
    if cli.pretty {
        config.pretty = true;
    }
    let result = dispatch(cli, &config).await;
    // AC-AESF-05: print error to stderr BEFORE returning exit code so the user
    // always gets a message. stdout is reserved for JSON receipt stream (SR-CLI-03).
    crate::error_mapper::print_error(&result);
    map_exit_code(&result)
}

/// Build the tokio runtime used by every async handler. Multi-threaded,
/// no extra background threads beyond the `tracing` async writer.
pub fn build_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build multi-thread tokio runtime")
}

/// Compose the early-init pipeline: config resolution + observability init.
pub fn early_init(argv: &[String]) -> Result<CliConfig, CliError> {
    let config_path = extract_config_override(argv);

    let inputs = ResolveInputs {
        cli_overrides: vec![],
        env_vars: std::env::vars().collect(),
        config_path,
    };

    // Resolve: compiled defaults → config.toml → LOOM_* env → CLI flags.
    crate::cli_config::resolve(inputs, None)
}

/// Extracts the `--config <path>` or `--config=<path>` value from raw
/// argv without a full clap parse. Used by `early_init` so the config
/// file path can be resolved before clap takes ownership of argv.
fn extract_config_override(argv: &[String]) -> Option<std::path::PathBuf> {
    let mut iter = argv.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--config" {
            return iter.next().map(std::path::PathBuf::from);
        }
        if let Some(val) = arg.strip_prefix("--config=") {
            return Some(std::path::PathBuf::from(val));
        }
    }
    None
}
