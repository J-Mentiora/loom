// CommandRouter — top-level clap-derived dispatch.
//
// # Contract semantics
// - **Subcommand → 1 RPC.** Every variant of the `Command`
//   enum corresponds to exactly one entry in the
//   subcommand-table. The 3 RPC-free local-action variants
//   (`Serve`, `Postinstall`, `Doctor`) plus `Version` and `Mcp` route
//   to local runners, never to `RpcClient`.
// - **clap derive.** `Command` is a `clap::Subcommand`; flag names
//   are mechanically derived from JSON-Schema field names by
//   `HelpGenerator`. No hand-curated names.
// - **Common preconditions.** `dispatch` enforces config-loaded and
//   observability-initialised before handing off to a handler.

use clap::{Parser, Subcommand};

use crate::action_commands::ActionArgs;
use crate::admin_commands::{GcArgs, McpArgs, PostinstallArgs, ServeArgs};
use crate::benchmark_commands::BenchmarkArgs;
use crate::chromium_downloader::{ChromiumDownloader, ChromiumDownloaderConfig};
use crate::chromium_pin;
use crate::cli_config::CliConfig;
use crate::cli_config::ColorChoice;
use crate::doctor_runner::{DoctorArgs, DoctorPaths};
use crate::import_commands::ImportPlaywrightArgs;
use crate::postinstall_runner::PostinstallOptions;
use crate::rpc_client::{RpcClient, RpcClientConfig};
use crate::serve_runner::ServeOptions;
use crate::session_commands::{
    AbortArgs, CloseArgs, CreateArgs, DiffArgs, ExportArgs, InspectArgs, ListArgs, ReapArgs,
    ReplayArgs, ValidateArgs,
};
use crate::vault_commands::{
    VaultAddArgs, VaultDeleteArgs, VaultDiagnoseArgs, VaultGrantArgs, VaultListArgs,
    VaultListLabelsArgs, VaultRevokeArgs,
};
use crate::version_command::LOOM_VERSION;
use crate::CliError;

/// Top-level CLI parser. Drives the clap derive pipeline.
#[derive(Debug, Parser)]
#[command(name = "loom", version = LOOM_VERSION, about = "Loom command-line interface")]
pub struct Cli {
    /// Override config file path. Highest precedence after CLI flags.
    #[arg(long, global = true)]
    pub config: Option<std::path::PathBuf>,

    /// Force human-readable colored multi-line output, even when stdout
    /// is piped. (Was indented JSON in earlier versions; see CHANGELOG.) The
    /// auto-detect default emits this format only when stdout is a TTY.
    #[arg(long, global = true)]
    pub pretty: bool,

    /// Force canonical JSON output, even when stdout is a TTY. Bypasses
    /// the auto-detected pretty-print path. Mutually
    /// exclusive with `--pretty`.
    #[arg(long, global = true)]
    pub json: bool,

    /// Suppress non-error output. For commands that produce a single
    /// resource (session create, action), prints only the canonical id.
    /// For list commands (session list, vault list), prints one id per
    /// line — may be a large amount of data on big result sets. Errors
    /// always go to stderr.
    #[arg(long, short = 'q', global = true)]
    pub quiet: bool,

    /// Color choice: `auto` (default), `always`, or `never`. Honours
    /// `NO_COLOR`, `CLICOLOR`, `CLICOLOR_FORCE`, and `TERM=dumb` env
    /// conventions in `auto` mode. Mutually exclusive with
    /// `--no-color`.
    #[arg(long, global = true, value_enum)]
    pub color: Option<ColorChoice>,

    /// Disable color output. Equivalent to `--color never`.
    #[arg(long = "no-color", global = true)]
    pub no_color: bool,

    #[command(subcommand)]
    pub command: Command,
}

/// Top-level command enum. One variant per subcommand family.
/// The subcommand → 1 RPC mapping is enforced by routing
/// each variant to exactly one handler in `dispatch`.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Session lifecycle commands (RPC).
    #[command(subcommand)]
    Session(SessionCmd),
    /// Action invocation (RPC).
    Action(ActionArgs),
    /// Vault commands (RPC). Only `add` may prompt.
    #[command(subcommand)]
    Vault(VaultCmd),
    /// Garbage collection (RPC).
    Gc(GcArgs),
    /// Spawn the daemon. RPC-free local action.
    Serve(ServeArgs),
    /// Idempotent installer. RPC-free local action.
    Postinstall(PostinstallArgs),
    /// Health checks. RPC-free local action.
    Doctor(DoctorArgs),
    /// MCP delegation entry. RPC-free; delegates to loom-mcp.
    #[command(name = "mcp")]
    Mcp(McpArgs),
    /// Import external recordings as non-replayable Loom sessions (RPC).
    #[command(subcommand)]
    Import(ImportCmd),
    /// Run performance benchmarks and validate SLAs. RPC-free local action.
    Benchmark(BenchmarkArgs),
}

/// Import subcommand — one variant per supported source format.
#[derive(Debug, Subcommand)]
pub enum ImportCmd {
    /// Import a Playwright trace.zip.
    Playwright(ImportPlaywrightArgs),
}

/// Session subcommand — exactly ten RPC-mapped variants.
#[derive(Debug, Subcommand)]
pub enum SessionCmd {
    Create(CreateArgs),
    Inspect(InspectArgs),
    List(ListArgs),
    Close(CloseArgs),
    Abort(AbortArgs),
    Replay(ReplayArgs),
    Diff(DiffArgs),
    Export(ExportArgs),
    Validate(ValidateArgs),
    /// `session reap [--apply]` — quarantine corrupt-WAL orphan sessions that
    /// are stuck in the active set and consuming concurrency slots. Previews by
    /// default (dry-run); pass `--apply` to actually move them aside.
    Reap(ReapArgs),
}

/// Vault subcommand. The four legacy `Grant/Revoke/List/Add` variants
/// plus v0.9.4 W6 `Delete/ListLabels/Diagnose` direct-credential
/// management.
#[derive(Debug, Subcommand)]
pub enum VaultCmd {
    Grant(VaultGrantArgs),
    Revoke(VaultRevokeArgs),
    List(VaultListArgs),
    Add(VaultAddArgs),
    /// `vault delete <label> [--force]` — remove a credential. Cascade
    /// revokes active grants when `--force` is set; otherwise blocks
    /// with `credential_in_use` if any grant references the label.
    Delete(VaultDeleteArgs),
    /// `vault list-labels` — enumerate stored credential labels. Distinct
    /// from `vault list` which lists *grants*.
    #[command(name = "list-labels")]
    ListLabels(VaultListLabelsArgs),
    /// `vault diagnose` — backend status + last-error snapshot. Output
    /// is a stable JSON schema (A-W6.4) for `jq` automation.
    Diagnose(VaultDiagnoseArgs),
}

/// Construct an RpcClient from the resolved CliConfig.
fn make_rpc_client(config: &CliConfig) -> RpcClient {
    RpcClient::new(RpcClientConfig {
        socket_path: config.socket_path.clone(),
        request_timeout: config.request_timeout,
    })
}

/// Routes the parsed `Cli` to the correct handler module. Returns the
/// outcome that `ErrorMapper::map_exit_code` translates into a process
/// exit code.
pub async fn dispatch(cli: Cli, config: &CliConfig) -> Result<(), CliError> {
    match cli.command {
        // RPC-free local actions.
        Command::Benchmark(args) => crate::benchmark_commands::run_benchmark(&args, config),

        // RPC-bearing subcommands — construct client lazily.
        Command::Session(cmd) => {
            let rpc = make_rpc_client(config);
            match cmd {
                SessionCmd::Create(a) => {
                    // First-run UX (AC5): surface a missing Chromium at session
                    // create — inline-download it (interactive) or print the
                    // precise remedy — instead of letting the first action
                    // cold-fail. No-op when Chromium already resolves.
                    crate::postinstall_runner::ensure_chromium_inline(
                        &config.chromium_dir,
                        chromium_pin::CHROMIUM_URL,
                        chromium_pin::CHROMIUM_SHA256,
                    )
                    .await?;
                    crate::session_commands::create(&rpc, config, a).await
                }
                SessionCmd::Inspect(a) => crate::session_commands::inspect(&rpc, config, a).await,
                SessionCmd::List(a) => crate::session_commands::list(&rpc, config, a).await,
                SessionCmd::Close(a) => crate::session_commands::close(&rpc, config, a).await,
                SessionCmd::Abort(a) => crate::session_commands::abort(&rpc, config, a).await,
                SessionCmd::Replay(a) => crate::session_commands::replay(&rpc, config, a).await,
                SessionCmd::Diff(a) => crate::session_commands::diff(&rpc, config, a).await,
                SessionCmd::Export(a) => crate::session_commands::export(&rpc, config, a).await,
                SessionCmd::Validate(a) => crate::session_commands::validate(&rpc, config, a).await,
                SessionCmd::Reap(a) => crate::session_commands::reap(&rpc, config, a).await,
            }
        }

        Command::Action(a) => {
            let rpc = make_rpc_client(config);
            let schemas = crate::schema_cache::SchemaCache::load(&config.schemas_dir)?;
            crate::action_commands::dispatch(&rpc, &schemas, config, a).await
        }

        Command::Vault(cmd) => {
            let rpc = make_rpc_client(config);
            match cmd {
                VaultCmd::Grant(a) => crate::vault_commands::grant(&rpc, config, a).await,
                VaultCmd::Revoke(a) => crate::vault_commands::revoke(&rpc, config, a).await,
                VaultCmd::List(a) => crate::vault_commands::list(&rpc, config, a).await,
                VaultCmd::Add(a) => crate::vault_commands::add(&rpc, config, a).await,
                VaultCmd::Delete(a) => crate::vault_commands::delete(&rpc, config, a).await,
                VaultCmd::ListLabels(a) => {
                    crate::vault_commands::list_labels(&rpc, config, a).await
                }
                VaultCmd::Diagnose(a) => crate::vault_commands::diagnose(&rpc, config, a).await,
            }
        }

        Command::Gc(a) => {
            let rpc = make_rpc_client(config);
            crate::admin_commands::gc(&rpc, config, a).await
        }

        // RPC-free local actions.
        Command::Serve(args) => {
            // First-run UX (AC5): a brand-new user's first command is usually
            // `loom serve` — make sure Chromium is present (inline-download it
            // with progress when interactive, else print the precise remedy)
            // before spawning the daemon. No-op when Chromium already resolves.
            crate::postinstall_runner::ensure_chromium_inline(
                &config.chromium_dir,
                chromium_pin::CHROMIUM_URL,
                chromium_pin::CHROMIUM_SHA256,
            )
            .await?;
            let opts = ServeOptions {
                socket_path: args.socket.unwrap_or_else(|| config.socket_path.clone()),
                config_path: args.config,
                daemon_binary: crate::serve_runner::default_daemon_binary()?,
            };
            crate::serve_runner::serve(opts).await.map(|_| ())
        }

        Command::Postinstall(args) => {
            // The loom-binaries install dir defaults to
            // dirs::data_local_dir()/loom/bin/. If unavailable (HOME unset),
            // fall back to current_exe().parent() so brew/manual paths still
            // succeed (they'll skip via co-location detection anyway).
            let loom_binaries_install_dir = crate::loom_binaries_downloader::default_install_dir()
                .unwrap_or_else(|| {
                    std::env::current_exe()
                        .ok()
                        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
                        .unwrap_or_else(|| std::path::PathBuf::from("."))
                });
            let opts = PostinstallOptions {
                surfaces_dir: config.surfaces_dir.clone(),
                schemas_dir: config.schemas_dir.clone(),
                chromium_url: chromium_pin::CHROMIUM_URL.to_string(),
                chromium_expected_sha256: chromium_pin::CHROMIUM_SHA256.to_string(),
                chromium_dir: config.chromium_dir.clone(),
                plist_path: std::path::PathBuf::from(
                    "/Library/LaunchDaemons/com.loom.daemon.plist",
                ),
                loom_binaries_version: env!("CARGO_PKG_VERSION").to_string(),
                loom_binaries_target_triple: crate::loom_binaries_downloader::host_target_triple()
                    .to_string(),
                loom_binaries_install_dir,
                skip_chromium: args.skip_chromium,
                skip_binaries: args.skip_binaries,
                man_install_dir: None,
            };
            crate::postinstall_runner::run(opts).await.map(|_| ())
        }

        Command::Doctor(_args) => {
            let rpc = make_rpc_client(config);
            // Per-OS layout — must match `loom postinstall` and the launch
            // resolver. `chromium_binary_subpath()` is the shared source of
            // truth so `loom doctor` looks where the archive actually
            // extracted (`chrome-linux/chrome` on Linux, not the macOS
            // `.app` bundle).
            let chromium_subpath = loom_shared::chromium_resolver::chromium_binary_subpath();
            let chromium = ChromiumDownloader::new(ChromiumDownloaderConfig {
                install_dir: config.chromium_dir.clone(),
                binary_subpath: chromium_subpath.clone(),
            });
            let paths = DoctorPaths {
                socket_path: config.socket_path.clone(),
                surfaces_dir: config.surfaces_dir.clone(),
                chromium_binary: config.chromium_dir.join(&chromium_subpath),
                chromium_expected_sha256: chromium_pin::CHROMIUM_SHA256.to_string(),
                keychain_label: "com.loom.auth".to_string(),
            };
            let result = crate::doctor_runner::run(&rpc, &chromium, &paths).await;
            // Always emit the DoctorReport JSON to stdout (pass or fail).
            let report = match &result {
                Ok(r) => r,
                Err(CliError::DoctorFailed(r)) => r,
                Err(_) => return result.map(|_| ()),
            };
            let value = serde_json::to_value(report)
                .unwrap_or_else(|_| serde_json::json!({"error":"serialization_failed"}));
            crate::output_formatter::emit_to_stdout("doctor", &value, config, None)?;
            // Advisory: soft-warn (NOT a check failure) when man
            // pages aren't installed at the resolved man dir. Non-fatal —
            // doctor's exit code is unchanged. Helps users notice why
            // `man loom` doesn't work.
            if crate::manpage_step::has_embedded_content() {
                if let Some(dir) = crate::manpage_step::resolve_install_dir() {
                    if !crate::manpage_step::man_pages_installed_at(&dir) {
                        eprintln!(
                            "advisory: man pages are not installed at {}; \
                             run `loom postinstall` so `man loom` works",
                            dir.display()
                        );
                    }
                }
            }
            result.map(|_| ())
        }

        Command::Mcp(_args) => crate::mcp_delegate::run().await,

        Command::Import(cmd) => {
            let rpc = make_rpc_client(config);
            match cmd {
                ImportCmd::Playwright(a) => {
                    crate::import_commands::import_playwright(&rpc, config, a).await
                }
            }
        }
    }
}
