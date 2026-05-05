// Re-export of the locked v5.3 interface. DO NOT EDIT here.
// Edit `systems/loom-cli/modules/VaultCommands/interfaces.rs` instead.
// VaultCommands — handlers for `loom vault.*` subcommands.
//
// # Contract semantics
// - **Routing.** Each subcommand maps to exactly one RPC:
//   `grant→vault.grant`, `revoke→vault.revoke`,
//   `list→vault.list_grants`, `add→vault.add`.
// - **No interactive prompts by default.** Only `add` may
//   prompt (OAuth device-flow); `--yes` opts out for non-interactive
//   shells. Clippy lint bans `dialoguer::*` outside this module.
// - **Hard binding 2 (vault stays in core).** `VaultCommands` never
//   handles raw secret bytes — OAuth tokens for `vault add` pass
//   through `RpcClient` to the daemon, redacted by `CliObservability`
//   before logging.

use clap::Args;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::cli_config::CliConfig;
use crate::output_formatter::emit_to_stdout;
use crate::rpc_client::RpcClient;
use crate::CliError;

/// Parse a TTL value. Accepts either a bare integer (interpreted as seconds)
/// or a humantime duration like "1h", "30m", "45s", "1h30m". This is the
/// `clap::value_parser` for the `--ttl` flag.
pub(crate) fn parse_ttl(s: &str) -> Result<u64, String> {
    if let Ok(n) = s.parse::<u64>() {
        return Ok(n);
    }
    humantime::parse_duration(s)
        .map(|d: Duration| d.as_secs())
        .map_err(|e| format!("invalid TTL '{s}': {e}; accepted: integer seconds or duration (1h, 30m, 45s, 1h30m)"))
}

/// `loom vault grant` arguments.
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct VaultGrantArgs {
    #[arg(long)]
    pub session: String,
    #[arg(long)]
    pub origin: String,
    /// Comma-separated list of OAuth scopes.
    #[arg(long)]
    pub scopes: String,
    /// Time-to-live: integer seconds OR humantime duration (1h, 30m, 45s).
    #[arg(long, value_parser = parse_ttl)]
    pub ttl: u64,
    #[arg(long)]
    pub label: Option<String>,
}

/// `loom vault revoke` arguments.
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct VaultRevokeArgs {
    #[arg(long)]
    pub grant: String,
    #[arg(long)]
    pub reason: Option<String>,
}

/// `loom vault list` arguments.
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct VaultListArgs {
    #[arg(long)]
    pub session: Option<String>,
}

/// `loom vault add <provider>` arguments. Only command in the CLI that
/// may prompt; respects `--yes` for non-interactive use.
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct VaultAddArgs {
    /// OAuth provider name (e.g. `google`, `github`).
    pub provider: String,
    #[arg(long)]
    pub label: Option<String>,
    /// Skip interactive confirmation; required in CI / scripts.
    #[arg(long)]
    pub yes: bool,
}

pub async fn grant(rpc: &RpcClient, cfg: &CliConfig, args: VaultGrantArgs) -> Result<(), CliError> {
    // Per loom-rpc_contract: vault.grant takes session_id / ttl_seconds
    // and `scopes` is a `Vec<String>` (CLI receives "--scopes a,b" comma-
    // separated; split + trim to match the contract shape). Clap arg
    // names (`--session` / `--ttl`) stay user-facing.
    let scopes: Vec<String> = args
        .scopes
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let resp = rpc
        .call(
            "vault.grant",
            serde_json::json!({
                "session_id": args.session,
                "origin": args.origin,
                "scopes": scopes,
                "ttl_seconds": args.ttl,
                "label": args.label.unwrap_or_default(),
            }),
        )
        .await?;
    emit_to_stdout("vault.grant", &resp, cfg, None)?;
    Ok(())
}

pub async fn revoke(
    rpc: &RpcClient,
    cfg: &CliConfig,
    args: VaultRevokeArgs,
) -> Result<(), CliError> {
    let resp = rpc
        .call(
            "vault.revoke",
            serde_json::json!({
                "grant_id": args.grant,
                "reason": args.reason,
            }),
        )
        .await?;
    emit_to_stdout("vault.revoke", &resp, cfg, None)?;
    Ok(())
}

pub async fn list(rpc: &RpcClient, cfg: &CliConfig, args: VaultListArgs) -> Result<(), CliError> {
    let resp = rpc
        .call(
            "vault.list_grants",
            serde_json::json!({
                "session_id": args.session,
            }),
        )
        .await?;
    // Map list_grants → vault.list for the curated registry (same shape).
    emit_to_stdout("vault.list", &resp, cfg, None)?;
    Ok(())
}

/// `vault add` — the SOLE handler permitted to prompt the user.
/// Implementation drives the OAuth device-flow; `--yes` short-circuits
/// confirmation for non-interactive shells.
pub async fn add(rpc: &RpcClient, cfg: &CliConfig, args: VaultAddArgs) -> Result<(), CliError> {
    let resp = rpc
        .call(
            "vault.add",
            serde_json::json!({
                "provider": args.provider,
                "label": args.label,
                "yes": args.yes,
            }),
        )
        .await?;
    emit_to_stdout("vault.add", &resp, cfg, None)?;
    Ok(())
}

/// Subcommand → RPC mapping table — `interface_tests` asserts coverage.
pub const SUBCOMMAND_RPC_MAP: &[(&str, &str)] = &[
    ("grant", "vault.grant"),
    ("revoke", "vault.revoke"),
    ("list", "vault.list_grants"),
    ("add", "vault.add"),
];
