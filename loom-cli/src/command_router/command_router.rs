// CommandRouter — top-level clap-derived dispatch.
//
// # Contract semantics
// - **IC-CLI-03 (subcommand → 1 RPC).** Every variant of the `Command`
//   enum corresponds to exactly one entry in design §6's
//   subcommand-table. The 3 RPC-free local-action variants
//   (`Serve`, `Postinstall`, `Doctor`) plus `Version` and `Mcp` route
//   to local runners, never to `RpcClient`.
// - **clap derive.** `Command` is a `clap::Subcommand`; flag names
//   are mechanically derived from JSON-Schema field names by
//   `HelpGenerator` (IC-CLI-09). No hand-curated names.
// - **Common preconditions.** `dispatch` enforces config-loaded and
//   observability-initialised before handing off to a handler.

use clap::{Parser, Subcommand};

use crate::action_commands::ActionArgs;
use crate::admin_commands::{GcArgs, McpArgs, PostinstallArgs, ServeArgs};
use crate::benchmark_commands::BenchmarkArgs;
use crate::chromium_downloader::{ChromiumDownloader, ChromiumDownloaderConfig};
use crate::chromium_pin;
use crate::cli_config::CliConfig;
use crate::doctor_runner::{DoctorArgs, DoctorPaths};
use crate::import_commands::ImportPlaywrightArgs;
use crate::postinstall_runner::PostinstallOptions;
use crate::rpc_client::{RpcClient, RpcClientConfig};
use crate::serve_runner::ServeOptions;
use crate::session_commands::{
    AbortArgs, CloseArgs, CreateArgs, DiffArgs, ExportArgs, InspectArgs, ListArgs, ReplayArgs,
    ValidateArgs,
};
use crate::vault_commands::{VaultAddArgs, VaultGrantArgs, VaultListArgs, VaultRevokeArgs};
use crate::CliError;

/// Top-level CLI parser. Drives the clap derive pipeline.
#[derive(Debug, Parser)]
#[command(name = "loom", version, about = "Loom command-line interface")]
pub struct Cli {
    /// Override config file path. Highest precedence after CLI flags.
    #[arg(long, global = true)]
    pub config: Option<std::path::PathBuf>,

    /// Pretty-print output via the schema-driven tabular renderer
    /// (IC-CLI-02). Default is canonical JSON (IC-CLI-01).
    #[arg(long, global = true)]
    pub pretty: bool,

    #[command(subcommand)]
    pub command: Command,
}

/// Top-level command enum. One variant per subcommand family.
/// IC-CLI-03's subcommand → 1 RPC mapping is enforced by routing
/// each variant to exactly one handler in `dispatch`.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Session lifecycle commands (RPC).
    #[command(subcommand)]
    Session(SessionCmd),
    /// Action invocation (RPC).
    Action(ActionArgs),
    /// Vault commands (RPC). Only `add` may prompt (IC-CLI-10).
    #[command(subcommand)]
    Vault(VaultCmd),
    /// Garbage collection (RPC).
    Gc(GcArgs),
    /// Spawn the daemon. RPC-free local action (IC-CLI-05).
    Serve(ServeArgs),
    /// Idempotent installer. RPC-free local action (IC-CLI-06).
    Postinstall(PostinstallArgs),
    /// Health checks. RPC-free local action (IC-CLI-07).
    Doctor(DoctorArgs),
    /// MCP delegation entry. RPC-free; delegates to loom-mcp
    /// (SR-CLI-02).
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
    /// Import a Playwright trace.zip. (AC-INTEROP-01.1)
    Playwright(ImportPlaywrightArgs),
}

/// Session subcommand — exactly nine RPC-mapped variants.
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
}

/// Vault subcommand — four RPC-mapped variants.
#[derive(Debug, Subcommand)]
pub enum VaultCmd {
    Grant(VaultGrantArgs),
    Revoke(VaultRevokeArgs),
    List(VaultListArgs),
    Add(VaultAddArgs),
}

/// Construct an RpcClient from the resolved CliConfig.
fn make_rpc_client(config: &CliConfig) -> RpcClient {
    RpcClient::new(RpcClientConfig {
        socket_path: config.socket_path.clone(),
        connect_timeout: config.connect_timeout,
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
                SessionCmd::Create(a) => crate::session_commands::create(&rpc, config, a).await,
                SessionCmd::Inspect(a) => crate::session_commands::inspect(&rpc, config, a).await,
                SessionCmd::List(a) => crate::session_commands::list(&rpc, config, a).await,
                SessionCmd::Close(a) => crate::session_commands::close(&rpc, config, a).await,
                SessionCmd::Abort(a) => crate::session_commands::abort(&rpc, config, a).await,
                SessionCmd::Replay(a) => crate::session_commands::replay(&rpc, config, a).await,
                SessionCmd::Diff(a) => crate::session_commands::diff(&rpc, config, a).await,
                SessionCmd::Export(a) => crate::session_commands::export(&rpc, config, a).await,
                SessionCmd::Validate(a) => crate::session_commands::validate(&rpc, config, a).await,
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
            }
        }

        Command::Gc(a) => {
            let rpc = make_rpc_client(config);
            crate::admin_commands::gc(&rpc, config, a).await
        }

        // RPC-free local actions.
        Command::Serve(args) => {
            let opts = ServeOptions {
                socket_path: args.socket.unwrap_or_else(|| config.socket_path.clone()),
                config_path: args.config,
                daemon_binary: crate::serve_runner::default_daemon_binary()?,
            };
            crate::serve_runner::serve(opts).await.map(|_| ())
        }

        Command::Postinstall(args) => {
            // AC-DIST-01: loom-binaries install dir defaults to
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
            };
            crate::postinstall_runner::run(opts).await.map(|_| ())
        }

        Command::Doctor(_args) => {
            let rpc = make_rpc_client(config);
            let chromium = ChromiumDownloader::new(ChromiumDownloaderConfig {
                install_dir: config.chromium_dir.clone(),
                binary_subpath: std::path::PathBuf::from("Chromium.app/Contents/MacOS/Chromium"),
            });
            let paths = DoctorPaths {
                socket_path: config.socket_path.clone(),
                surfaces_dir: config.surfaces_dir.clone(),
                chromium_binary: config
                    .chromium_dir
                    .join("Chromium.app/Contents/MacOS/Chromium"),
                chromium_expected_sha256: chromium_pin::CHROMIUM_SHA256.to_string(),
                keychain_label: "com.loom.auth".to_string(),
            };
            let result = crate::doctor_runner::run(&rpc, &chromium, &paths).await;
            // IC-CLI-07: always emit the DoctorReport JSON to stdout (pass or fail).
            let report = match &result {
                Ok(r) => r,
                Err(CliError::DoctorFailed(r)) => r,
                Err(_) => return result.map(|_| ()),
            };
            println!(
                "{}",
                serde_json::to_string(report)
                    .unwrap_or_else(|_| r#"{"error":"serialization_failed"}"#.to_string())
            );
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
