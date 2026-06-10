// ImportCommands — `loom import playwright <trace.zip>` handler.
//
// # Contract semantics
// - One subcommand family: `loom import playwright <TRACE_ZIP_PATH>`
// - Reads trace.zip bytes locally; hex-encodes; sends via `import.playwright` RPC.
// - Daemon decodes hex, passes raw bytes to `PlaywrightImporter::import()`.
// - Response forwarded verbatim to `OutputFormatter` (receipt pass-through).
// - Exit codes per loom-cli_contract.md: 0 = ok, 1 = error receipt, 2 = usage error.
//
// # RPC integration note
// `import.playwright` RPC method registration lives in loom-rpc and is a
// follow-up integration task. The CLI → daemon path will be tested
// end-to-end in wire-boundary tests.

use clap::Args;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::cli_config::CliConfig;
use crate::output_formatter::emit_to_stdout;
use crate::rpc_client::RpcClient;
use crate::CliError;

/// `loom import playwright <TRACE_ZIP_PATH>` arguments.
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct ImportPlaywrightArgs {
    /// Path to the Playwright trace.zip file to import.
    pub trace_path: PathBuf,
}

/// Headroom reserved for the JSON-RPC envelope around `trace_hex`
/// (`{"jsonrpc":"2.0","method":"import.playwright","params":...,"id":1}`
/// is < 100 bytes; 1 KiB is comfortably conservative).
const IMPORT_ENVELOPE_OVERHEAD_BYTES: usize = 1024;

/// Largest trace.zip importable over the current wire protocol.
///
/// The daemon's `LengthDelimitedCodec` rejects frames over
/// `MAX_FRAME_BYTES` (16 MiB) and the trace is hex-encoded (2x) into a
/// single `import.playwright` frame, so the effective cap is just under
/// half the frame cap. Checked BEFORE connecting so an oversized trace
/// gets an actionable receipt instead of the misleading connection
/// error a dropped oversized frame used to surface as.
pub fn max_importable_trace_bytes() -> usize {
    (loom_rpc::frame_handler::frame_handler::MAX_FRAME_BYTES - IMPORT_ENVELOPE_OVERHEAD_BYTES) / 2
}

/// Handler for `loom import playwright`. Reads trace bytes locally, hex-encodes,
/// sends to daemon via `import.playwright` RPC, forwards receipt to stdout.
pub async fn import_playwright(
    rpc: &RpcClient,
    cfg: &CliConfig,
    args: ImportPlaywrightArgs,
) -> Result<(), CliError> {
    // typed error envelope when the trace file is missing
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

    // Pre-flight the wire-size cap before any connection or hex
    // allocation: a trace over ~8 MiB hex-encodes past the daemon's
    // 16 MiB frame cap, which drops the connection with a generic
    // connection error pointing at the wrong fix ("no daemon running").
    let max_trace = max_importable_trace_bytes();
    if bytes.len() > max_trace {
        return Err(CliError::Receipt(serde_json::json!({
            "status": "error",
            "code": "trace_too_large",
            "message": format!(
                "playwright trace at {} is {} bytes; the daemon's 16 MiB \
                 frame cap limits hex-encoded imports to {} bytes. \
                 Re-record the trace without screenshots/snapshots \
                 (e.g. `tracing.start({{ screenshots: false, snapshots: false }})`) \
                 or split the recording into smaller traces.",
                args.trace_path.display(),
                bytes.len(),
                max_trace,
            ),
            "data": {
                "path": args.trace_path.display().to_string(),
                "trace_bytes": bytes.len(),
                "max_trace_bytes": max_trace,
            },
        })));
    }

    let trace_hex = hex::encode(&bytes);

    let resp = rpc
        .call(
            "import.playwright",
            serde_json::json!({ "trace_hex": trace_hex }),
        )
        .await?;

    // Forward raw receipt to stdout (verbatim; TTY mode selection
    // happens inside emit).
    emit_to_stdout("session.import", &resp, cfg, None)?;
    Ok(())
}
