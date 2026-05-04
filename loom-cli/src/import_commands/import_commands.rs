// ImportCommands — `loom import playwright <trace.zip>` handler.
//
// # Contract semantics
// - One subcommand family: `loom import playwright <TRACE_ZIP_PATH>`
// - Reads trace.zip bytes locally; hex-encodes; sends via `import.playwright` RPC.
// - Daemon decodes hex, passes raw bytes to `PlaywrightImporter::import()`.
// - Response forwarded verbatim to `OutputFormatter` (receipt pass-through).
// - Exit codes per loom-cli_contract.md: 0 = ok, 1 = error receipt, 2 = usage error.
//
// # Phase 7 note
// `import.playwright` RPC method registration lives in loom-rpc — Phase 7 integration
// task. The CLI → daemon path is tested end-to-end in Phase 7 wire-boundary tests.

use clap::Args;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::cli_config::CliConfig;
use crate::output_formatter::format_output;
use crate::rpc_client::RpcClient;
use crate::CliError;

/// `loom import playwright <TRACE_ZIP_PATH>` arguments.
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct ImportPlaywrightArgs {
    /// Path to the Playwright trace.zip file to import.
    pub trace_path: PathBuf,
}

/// Handler for `loom import playwright`. Reads trace bytes locally, hex-encodes,
/// sends to daemon via `import.playwright` RPC, forwards receipt to stdout.
pub async fn import_playwright(
    rpc: &RpcClient,
    cfg: &CliConfig,
    args: ImportPlaywrightArgs,
) -> Result<(), CliError> {
    // AC-PWIMPORT-01: typed error envelope when the trace file is missing
    // or unreadable, instead of leaking the raw `os error 2`. Use Receipt
    // (exit 1) rather than Internal (exit 2): the file argument is well-
    // formed at the clap level — the user supplied a path, it just doesn't
    // exist or isn't readable. That's a runtime error, not a usage error.
    let bytes = std::fs::read(&args.trace_path).map_err(|e| {
        let kind = match e.kind() {
            std::io::ErrorKind::NotFound => "trace_not_found",
            std::io::ErrorKind::PermissionDenied => "trace_permission_denied",
            _ => "trace_read_failed",
        };
        CliError::Receipt(serde_json::json!({
            "status": "error",
            "code": kind,
            "message": format!(
                "cannot read playwright trace at {}: {e}",
                args.trace_path.display()
            ),
            "data": {
                "path": args.trace_path.display().to_string(),
                "io_kind": format!("{:?}", e.kind()),
            },
        }))
    })?;

    let trace_hex = hex::encode(&bytes);

    let resp = rpc
        .call("import.playwright", serde_json::json!({ "trace_hex": trace_hex }))
        .await?;

    // Forward raw receipt to stdout (canonical JSON, no rewriting — IC-CLI-03).
    println!("{}", format_output(&resp, cfg.pretty)?);
    Ok(())
}
