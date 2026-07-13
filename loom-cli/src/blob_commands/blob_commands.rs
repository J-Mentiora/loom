// BlobCommands — content-addressed blob retrieval.
//
// `loom blob get <hash>` fetches raw blob bytes from the daemon's
// content store via the `content.get` RPC — the same flow
// `session_commands::export` uses — and writes them to `--output` or
// stdout. Binary output to an interactive terminal is refused
// (docker-save precedent); `-o -` is the explicit stdout sentinel.

use std::io::Write as _;
use std::path::PathBuf;

use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

use crate::cli_config::CliConfig;
use crate::rpc_client::RpcClient;
use crate::CliError;

/// `loom blob` subcommand family.
#[derive(Debug, Subcommand)]
pub enum BlobCmd {
    /// Fetch a stored blob by content hash and write it to a file or stdout.
    Get(BlobGetArgs),
}

/// `loom blob get <hash>` arguments.
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct BlobGetArgs {
    /// Content hash (64-char sha256 hex) of the blob, e.g. the
    /// `audio_after_hash` printed by `loom action web.stop_audio_capture`.
    pub hash: String,
    /// Output file path (an existing file is overwritten). `-` writes to
    /// stdout. Omitted: bytes go to stdout when it is a pipe/redirect;
    /// an interactive terminal is refused.
    #[arg(short = 'o', long)]
    pub output: Option<PathBuf>,
}

pub async fn get(rpc: &RpcClient, _cfg: &CliConfig, args: BlobGetArgs) -> Result<(), CliError> {
    // The daemon's ref validation is lowercase-only; normalize the one
    // plausible paste variant instead of bouncing it back to the user.
    let hash = args.hash.trim().to_ascii_lowercase();

    let resp = rpc
        .call("content.get", serde_json::json!({ "artifact_ref": hash }))
        .await?;

    #[derive(serde::Deserialize)]
    struct ContentData {
        data_hex: String,
    }
    let content: ContentData = serde_json::from_value(resp)
        .map_err(|e| CliError::Internal(format!("content response parse: {e}")))?;

    let bytes = hex::decode(&content.data_hex)
        .map_err(|e| CliError::Internal(format!("hex decode: {e}")))?;

    use std::io::IsTerminal as _;
    match resolve_output(args.output, std::io::stdout().is_terminal())? {
        OutputTarget::File(path) => std::fs::write(&path, &bytes)
            .map_err(|e| CliError::Internal(format!("write {}: {e}", path.display()))),
        OutputTarget::Stdout => std::io::stdout()
            .lock()
            .write_all(&bytes)
            .map_err(|e| CliError::Internal(e.to_string())),
    }
}

/// Where the fetched bytes go.
#[derive(Debug, PartialEq)]
pub(crate) enum OutputTarget {
    File(PathBuf),
    Stdout,
}

/// Output-resolution matrix, pure so the TTY branch is unit-testable:
/// `-o <file>` → file (overwrite) · `-o -` → stdout · no `-o` + pipe →
/// stdout · no `-o` + terminal → refuse (usage error, exit 2).
pub(crate) fn resolve_output(
    output: Option<PathBuf>,
    stdout_is_tty: bool,
) -> Result<OutputTarget, CliError> {
    match output {
        Some(p) if p.as_os_str() == "-" => Ok(OutputTarget::Stdout),
        Some(path) => Ok(OutputTarget::File(path)),
        None if stdout_is_tty => Err(CliError::Usage(
            "refusing to write binary data to a terminal; use `-o <file>`, `-o -`, or redirect stdout"
                .to_string(),
        )),
        None => Ok(OutputTarget::Stdout),
    }
}
