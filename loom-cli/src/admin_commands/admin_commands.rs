// Re-export of the locked v5.3 interface. DO NOT EDIT here.
// Edit `systems/loom-cli/modules/AdminCommands/interfaces.rs` instead.
// AdminCommands — `gc` (RPC), `serve` / `postinstall` / `doctor`
// (local), `mcp serve` (delegate), `--version` (special).
//
// # Contract semantics
// - **Routing.** `gc → gc.run` (RPC); `serve`, `postinstall`,
//   `doctor` are the 3 RPC-free local-action subcommands; `mcp serve`
//   is the delegate path; `--version` bypasses everything.
// - **Dispatcher only.** This module routes; the real work lives in
//   `ServeRunner`, `PostinstallRunner`, `DoctorRunner`, `McpDelegate`,
//   `VersionCommand`.

use clap::Args;
use serde::{Deserialize, Serialize};

use crate::cli_config::CliConfig;
use crate::output_formatter::emit_to_stdout;
use crate::rpc_client::RpcClient;
use crate::CliError;

/// `loom gc` arguments.
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct GcArgs {
    /// Time-to-live in days for completed sessions.
    #[arg(long)]
    pub ttl: Option<u64>,
    /// Maximum store size in bytes.
    #[arg(long = "store-max-bytes")]
    pub store_max_bytes: Option<u64>,
}

/// `loom serve` arguments. Forwarded to `ServeRunner::serve`.
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct ServeArgs {
    #[arg(long)]
    pub socket: Option<std::path::PathBuf>,
    #[arg(long)]
    pub config: Option<std::path::PathBuf>,
}

/// `loom postinstall` arguments. Forwarded to `PostinstallRunner::run`.
///
/// Two skip flags so a brew/manual user (with the 3
/// sibling binaries already co-located) can `loom postinstall --skip-binaries`
/// to avoid the GH Release fetch entirely. The detection in
/// `loom_binaries_step` handles the no-flag case automatically (sentinel
/// + presence check), so most users never need these flags.
#[derive(Debug, Clone, Args, Serialize, Deserialize, Default)]
pub struct PostinstallArgs {
    /// Skip the Chromium download + verification step.
    #[arg(long)]
    pub skip_chromium: bool,
    /// Skip the loom-daemon / loom-mcp / loom-shim-chromium download
    /// from the matching GitHub Release.
    #[arg(long)]
    pub skip_binaries: bool,
}

/// `loom mcp serve` arguments. Forwarded to `McpDelegate::run`.
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct McpArgs {
    /// MCP delegation accepts no flags at v1 — stdio is the sole
    /// transport per loom-mcp_contract.md.
    #[arg(skip)]
    pub _placeholder: (),
}

#[allow(clippy::derivable_impls)]
impl Default for McpArgs {
    fn default() -> Self {
        Self { _placeholder: () }
    }
}

/// `loom gc` handler — the SOLE RPC-bearing subcommand under
/// `AdminCommands`. Maps to RPC method `gc.run`.
pub async fn gc(rpc: &RpcClient, cfg: &CliConfig, args: GcArgs) -> Result<(), CliError> {
    let resp = rpc
        .call(
            "gc.run",
            serde_json::json!({
                "ttl_days": args.ttl,
                "store_max_bytes": args.store_max_bytes,
            }),
        )
        .await?;
    emit_to_stdout("gc.run", &resp, cfg, None)?;
    Ok(())
}

/// `loom --version` dispatcher. Bypasses RPC entirely;
/// p99 ≤ 200 ms. Concrete logic lives in `VersionCommand`.
pub fn version_dispatch() -> Result<(), CliError> {
    crate::version_command::print()
}

/// Subcommand mapping for AdminCommands. The 3 RPC-free entries are
/// flagged with `local:`.
pub const SUBCOMMAND_MAP: &[(&str, &str)] = &[
    ("gc", "gc.run"),
    ("serve", "local:ServeRunner"),
    ("postinstall", "local:PostinstallRunner"),
    ("doctor", "local:DoctorRunner"),
    ("mcp serve", "local:McpDelegate"),
    ("--version", "local:VersionCommand"),
];
