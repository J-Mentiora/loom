// McpMain — `loom mcp serve` clap subcommand and process entrypoint.
// Types only. Free functions implemented in mod.rs.

use clap::Parser;
use std::path::PathBuf;
use std::time::Duration;

/// `loom mcp serve` clap subcommand.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "serve",
    about = "Run the Loom MCP server (stdio transport).",
    long_about = "Reads MCP framed JSON-RPC from stdin, dispatches to the \
                  Loom daemon over a Unix socket, writes responses to stdout. \
                  Operates zero-config; see `loom doctor` for daemon health."
)]
pub struct ServeArgs {
    /// Path to the daemon's HELLO token artefact.
    #[arg(long, env = "LOOM_HELLO_TOKEN_PATH")]
    pub hello_token_path: Option<PathBuf>,

    /// Override the daemon socket path.
    #[arg(long, env = "LOOM_SOCKET_PATH")]
    pub socket_path: Option<PathBuf>,

    /// Disable vault-argument redaction.
    #[arg(long, default_value_t = false)]
    pub no_vault_redaction: bool,
}

/// Drain timeout when shutdown is requested.
pub const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
