// `main` — `loom` binary entrypoint.
//
// # Contract semantics
// - **Entry layer.** Builds the clap parser, initialises the tokio
//   runtime, resolves config, initialises observability, then hands
//   off to `CommandRouter`.
// - **Exit codes.** `main` is the SOLE caller of
//   `std::process::exit`. All exits are mapped through
//   `ErrorMapper::map_exit_code`. The clippy lint
//   `// FORBIDDEN: std::process::exit outside main + ErrorMapper`
//   enforces this elsewhere in the crate.
// - **Config precedence.** `main` invokes
//   `ConfigResolver::resolve(args, env, file)` before constructing
//   `CommandRouter`. The resolved `CliConfig` is owned by `main` and
//   borrowed by handlers via `CommandRouter`.
// - **Tokio runtime.** A single multi-threaded `tokio` runtime is
//   constructed in `main` and dropped at exit. No nested runtimes.
//
// # module_kind
// Binary entry — exports `fn main()` and the `loom-cli` crate's
// library-style `run` helper for integration tests.

use clap::{CommandFactory as _, FromArgMatches as _};
use std::io::IsTerminal;

use crate::cli_config::output_mode::OutputMode;
use crate::cli_config::{resolve_color, ColorChoice, ResolveInputs};
use crate::command_router::{dispatch, validate_flags, Cli};
use crate::error_mapper::map_exit_code;
use crate::CliConfig;
use crate::CliError;

/// Process entrypoint. Parses argv, builds the runtime, dispatches to
/// `CommandRouter`, and exits with the mapped exit code from
/// `ErrorMapper`.
///
/// SOLE caller of `std::process::exit` in the crate
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
    // `loom action <name> --help` must show the registered
    // action's detailed signature. clap's built-in --help intercepts at
    // the subcommand level (renders Action's help) before our dispatch
    // can see `extra`, so we pre-scan argv here. Pre-clap so the test
    // harness gets the same behaviour.
    if let Some(method) = detect_per_action_help_request(&argv) {
        if let Some(meta) = loom_rpc::action_registry::find(&method) {
            print!("{}", crate::action_help::render_per_action_help(meta));
            return 0;
        }
    }

    // Parse first — so --help / --version never trigger config I/O
    // (--version p99 ≤ 200 ms, must not read filesystem).
    // Use try_parse_from so the test harness can call run() without
    // triggering std::process::exit.
    //
    // Augment the `action` subcommand with a registry-rendered after_help
    // so `loom action --help` lists every registered action
    // with a one-line summary. The leak is one-shot per process.
    let after_help: &'static str =
        Box::leak(crate::action_help::render_all_actions_after_help().into_boxed_str());
    let cmd = Cli::command()
        .mut_subcommand("action", |sub| sub.after_help(after_help))
        .mut_subcommand("action", |sub| sub.after_long_help(after_help));
    let cli = match cmd.try_get_matches_from(argv.iter().map(String::as_str)) {
        Ok(matches) => match Cli::from_arg_matches(&matches) {
            Ok(cli) => cli,
            Err(e) => {
                let code = e.exit_code();
                let _ = e.print();
                return code;
            }
        },
        Err(e) => {
            // exit_code 0 → DisplayHelp / DisplayVersion (print to stdout/stderr)
            // exit_code 2 → usage / parse error
            let code = e.exit_code();
            let _ = e.print(); // prints to the appropriate stream
            return code;
        }
    };

    // D-7 / D-31: reject conflicting flags BEFORE config
    // resolution so the failure mode is a fast Usage error.
    if let Err(e) = validate_flags(&cli) {
        let r = Err(e);
        crate::error_mapper::print_error(&r);
        return map_exit_code(&r);
    }

    // ConfigResolver::resolve called before CommandRouter.
    let config = match early_init(&argv) {
        Ok(c) => c,
        Err(e) => {
            // print_error before exit so early-init failures are visible.
            let r = Err(e);
            crate::error_mapper::print_error(&r);
            return map_exit_code(&r);
        }
    };

    // resolve output_mode per D-7
    // precedence (quiet > json > pretty > auto-detect) and per-stream
    // color enablement per D-20 / D-22.
    let mut config = config;
    config.pretty = merge_pretty_flag(config.pretty, cli.pretty);
    let stdout_is_tty = std::io::stdout().is_terminal();
    let stderr_is_tty = std::io::stderr().is_terminal();
    config.output_mode = OutputMode::resolve(cli.quiet, cli.json, config.pretty, stdout_is_tty);
    let color_choice = if cli.no_color {
        ColorChoice::Never
    } else {
        cli.color.unwrap_or(ColorChoice::Auto)
    };
    config.stdout_color_enabled = resolve_color(color_choice, stdout_is_tty);
    config.stderr_color_enabled = resolve_color(color_choice, stderr_is_tty);
    let result = dispatch(cli, &config).await;
    // print error to stderr BEFORE returning exit code so the user
    // always gets a message. stdout is reserved for JSON receipt stream.
    // D-20: color the prose RED+BOLD when stderr is a TTY.
    crate::error_mapper::print_error_with_color(&result, config.stderr_color_enabled);
    map_exit_code(&result)
}

/// Merge the `--pretty` flag into the file/env-resolved `pretty` value.
///
/// `--pretty` is a clap `SetTrue` boolean: present means `true`, absent
/// means "not passed" (never "force off"). So the flag may only force
/// pretty ON; when absent, the value resolved from config.toml /
/// `LOOM_PRETTY` must survive (documented precedence: CLI > env > file >
/// defaults). The previous unconditional `config.pretty = cli.pretty`
/// clobbered the resolved value with `false`, making the file/env knobs
/// dead. `--quiet` / `--json` still take precedence downstream in
/// `OutputMode::resolve`.
pub fn merge_pretty_flag(resolved_pretty: bool, pretty_flag: bool) -> bool {
    resolved_pretty || pretty_flag
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

/// Returns `Some(method)` if argv looks like `loom action <method> ... --help`
/// (or `-h`) where `method` is a non-flag positional. Used to short-circuit
/// to the registry-driven per-action help renderer before clap consumes
/// `--help` at the subcommand level.
///
/// `--help`/`-h` is honored ONLY in KEY position. The per-action args use
/// `allow_hyphen_values` (see `action_commands::parse_extra_to_json_typed`),
/// so a `-h` that is the VALUE of a preceding `--key` — e.g. typing the
/// literal text `--text -h` — must flow through as the action argument, NOT
/// short-circuit to help with exit 0 (audit 2026-06-10, F81).
fn detect_per_action_help_request(argv: &[String]) -> Option<String> {
    let mut iter = argv.iter().skip(1).peekable();
    if iter.next().map(String::as_str) != Some("action") {
        return None;
    }
    // Locate the method positional (first non-flag token after `action`),
    // then scan only the tokens AFTER it for a key-position help flag.
    let after_action: Vec<&String> = iter.collect();
    let method_idx = after_action.iter().position(|a| !a.starts_with('-'))?;
    let method = after_action[method_idx];

    // Walk the post-method tokens, pairing `--key` with its value the same
    // way `parse_extra_to_json_typed` does: a token following a `--key` that
    // does not itself start with `--` is consumed as that key's value and so
    // is NOT a flag position. Only a help token that is itself a key counts.
    let post_method = &after_action[method_idx + 1..];
    let mut has_help = false;
    let mut i = 0;
    while i < post_method.len() {
        let tok = post_method[i].as_str();
        if tok == "--help" || tok == "-h" {
            has_help = true;
            break;
        }
        if tok.starts_with("--") {
            // A `--key`: the next token is its VALUE iff it does not itself
            // start with `--` (mirrors parse_extra_to_json_typed). That value
            // — even a literal `-h` — is then skipped, never read as a flag.
            if let Some(next) = post_method.get(i + 1) {
                if !next.starts_with("--") {
                    i += 2;
                    continue;
                }
            }
        }
        i += 1;
    }

    if has_help {
        Some(method.clone())
    } else {
        None
    }
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
