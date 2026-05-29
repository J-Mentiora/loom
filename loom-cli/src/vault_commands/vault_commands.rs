// VaultCommands — handlers for `loom vault.*` subcommands.
//
// # Contract semantics
// - **Routing.** Each subcommand maps to exactly one RPC:
//   `grant→vault.grant`, `revoke→vault.revoke`,
//   `list→vault.list_grants`, `add→vault.add`.
// - **No interactive prompts by default.** Only `add` may
//   prompt (OAuth device-flow); `--yes` opts out for non-interactive
//   shells. Clippy lint bans `dialoguer::*` outside this module.
// - **Vault stays in core.** `VaultCommands` never handles raw secret
//   bytes — OAuth tokens for `vault add` pass through `RpcClient` to
//   the daemon, redacted by `CliObservability` before logging.

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

/// `loom vault add <provider>` arguments. Two flows:
///
/// 1. **OAuth device flow** (existing) — `loom vault add <provider>`
///    where `provider` is OAuth-allowlisted (`github` today). The CLI
///    drives the device-flow handshake.
/// 2. **Direct credential injection** (v0.9.4 W6) — `loom vault add
///    --label <name> --from-stdin` OR `--from-file <path>`. Reads raw
///    bytes (binary-safe per A-W6.2), validates `--label` per D37,
///    and dispatches `vault.set_secret`. Mutually exclusive with the
///    positional `provider`.
///
/// `--overwrite` enables the W6 replace-existing path; without it,
/// the daemon returns `AlreadyExists` (exit 5 per D33). `--yes` is
/// honored by both flows for non-interactive use.
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct VaultAddArgs {
    /// OAuth provider name (e.g. `github`). Required when no
    /// `--from-stdin` / `--from-file` is set; rejected when either is.
    #[arg(value_parser, required = false)]
    pub provider: Option<String>,
    /// Label under which to store the credential. Required for the
    /// direct injection flow (with `--from-stdin` / `--from-file`);
    /// optional for the OAuth flow (defaults to the provider name).
    #[arg(long)]
    pub label: Option<String>,
    /// Read raw secret bytes from stdin until EOF (A-W6.2). Mutually
    /// exclusive with `--from-file` and the OAuth flow.
    #[arg(long)]
    pub from_stdin: bool,
    /// Read raw secret bytes from `<path>` (`-` reads stdin). Mutually
    /// exclusive with `--from-stdin` and the OAuth flow.
    #[arg(long, value_name = "PATH")]
    pub from_file: Option<String>,
    /// Replace an existing credential under the same label. Without
    /// this flag, the daemon returns `AlreadyExists` (exit 5).
    #[arg(long)]
    pub overwrite: bool,
    /// Optional session id; in-session calls write G5a/G5b audit
    /// entries to the named session's manifest. Sessionless `vault add`
    /// (the default) records only via `tracing::info!`.
    #[arg(long)]
    pub session: Option<String>,
    /// Skip interactive confirmation; required in CI / scripts.
    #[arg(long)]
    pub yes: bool,
    /// v0.9.6 web-cookie-injection: credential type. `oauth` (default,
    /// backward compatible) goes through the OAuth provider /
    /// direct-credential flows. `cookie` reads a vault cookie blob
    /// (JSON `{"schema_version":1, "cookies":[NetworkCookieParam, ...]}`),
    /// resolves the operator's current session via
    /// `vault.get_session_context` (or uses --session when provided),
    /// stores the bytes with `vault.set_secret`, and issues a Cookie
    /// grant via `vault.grant`. Prints the resulting GrantId to stdout
    /// so the operator can paste it into MCP `set_cookies({grant_id})`
    /// calls.
    #[arg(long, value_name = "TYPE", default_value = "oauth")]
    pub credential_type: String,
}

/// v0.9.6: cookie blob JSON schema validation. Returns a typed CliError
/// on parse failure or schema-version mismatch. The bytes themselves
/// are not retained; the caller passes them onward to `vault.set_secret`.
pub(super) fn validate_cookie_blob_json(bytes: &[u8]) -> Result<(), CliError> {
    let v: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| CliError::Usage(format!("invalid cookie blob JSON: {e}")))?;
    let schema_version = v
        .get("schema_version")
        .and_then(|n| n.as_u64())
        .ok_or_else(|| {
            CliError::Usage("cookie blob missing `schema_version: <int>` field".to_string())
        })?;
    if schema_version != 1 {
        return Err(CliError::Usage(format!(
            "unsupported cookie blob schema_version: {schema_version} (expected 1)"
        )));
    }
    let cookies = v
        .get("cookies")
        .ok_or_else(|| CliError::Usage("cookie blob missing `cookies: [...]` field".to_string()))?;
    if !cookies.is_array() {
        return Err(CliError::Usage(
            "cookie blob `cookies` field must be a JSON array".to_string(),
        ));
    }
    // Light per-cookie shape check: each item must be an object with
    // a `name` string. Deeper validation happens daemon-side via
    // `validate_cookie_params` (cookie_types.rs).
    for (i, c) in cookies.as_array().unwrap().iter().enumerate() {
        if !c.is_object() {
            return Err(CliError::Usage(format!(
                "cookie blob entry {i} is not a JSON object"
            )));
        }
        if c.get("name").and_then(|n| n.as_str()).is_none() {
            return Err(CliError::Usage(format!(
                "cookie blob entry {i} missing `name: \"...\"` field"
            )));
        }
    }
    Ok(())
}

/// `loom vault delete <label>` arguments (W6.3).
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct VaultDeleteArgs {
    /// Label of the credential to delete.
    pub label: String,
    /// Cascade-revoke active grants by `label` before deleting (D29).
    /// Without this flag, an active grant blocks the delete with
    /// `VaultRejection { code: credential_in_use }`.
    #[arg(long)]
    pub force: bool,
    #[arg(long)]
    pub session: Option<String>,
}

/// `loom vault list-labels` arguments (W6.4).
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct VaultListLabelsArgs {
    #[arg(long)]
    pub session: Option<String>,
}

/// `loom vault diagnose` arguments (W6.5).
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct VaultDiagnoseArgs {}

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

/// D37 canonical label policy enforced at the CLI boundary. The
/// daemon's wire boundary + manifest-writer (W5.10) re-validate as
/// belt-and-suspenders.
fn validate_label_cli(label: &str) -> Result<(), CliError> {
    if label.is_empty() {
        return Err(CliError::Usage("label is empty".into()));
    }
    if label.len() > 64 {
        return Err(CliError::Usage(format!(
            "label exceeds 64 chars ({} chars)",
            label.len()
        )));
    }
    if !label
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == ':' || c == '_' || c == '-')
    {
        return Err(CliError::Usage(format!(
            "label {label:?} fails canonical validation ^[A-Za-z0-9:_-]{{1,64}}$"
        )));
    }
    Ok(())
}

/// Read raw bytes until EOF per A-W6.2 — no newline stripping, no
/// charset conversion. Caps at 1 MiB (D22) before sending.
fn read_secret_bytes(args: &VaultAddArgs) -> Result<Vec<u8>, CliError> {
    use std::io::Read;
    const MAX_BYTES: usize = 1 << 20; // 1 MiB
    let mut buf = Vec::new();
    if args.from_stdin {
        std::io::stdin()
            .lock()
            .take((MAX_BYTES + 1) as u64)
            .read_to_end(&mut buf)
            .map_err(|e| CliError::Usage(format!("read stdin: {e}")))?;
    } else if let Some(path) = &args.from_file {
        if path == "-" {
            std::io::stdin()
                .lock()
                .take((MAX_BYTES + 1) as u64)
                .read_to_end(&mut buf)
                .map_err(|e| CliError::Usage(format!("read stdin: {e}")))?;
        } else {
            let f = std::fs::File::open(path)
                .map_err(|e| CliError::Usage(format!("open {path}: {e}")))?;
            std::io::BufReader::new(f)
                .take((MAX_BYTES + 1) as u64)
                .read_to_end(&mut buf)
                .map_err(|e| CliError::Usage(format!("read {path}: {e}")))?;
        }
    } else {
        return Err(CliError::Usage(
            "internal: read_secret_bytes called without --from-stdin or --from-file".into(),
        ));
    }
    if buf.len() > MAX_BYTES {
        return Err(CliError::Usage(format!(
            "secret exceeds 1 MiB cap ({} bytes)",
            buf.len()
        )));
    }
    if buf.is_empty() {
        return Err(CliError::Usage("secret is empty".into()));
    }
    Ok(buf)
}

/// `vault add` — dispatches to either the W6 direct-injection flow
/// (when `--from-stdin` / `--from-file` is set) or the legacy OAuth
/// device flow (when a positional `provider` is given).
pub async fn add(rpc: &RpcClient, cfg: &CliConfig, args: VaultAddArgs) -> Result<(), CliError> {
    let direct = args.from_stdin || args.from_file.is_some();

    // v0.9.6 cookie flow: --credential-type cookie REQUIRES direct
    // injection (--from-file / --from-stdin); the OAuth-flow positional
    // `provider` is rejected.
    if args.credential_type == "cookie" {
        if args.provider.is_some() {
            return Err(CliError::Usage(
                "`vault add <provider>` and --credential-type cookie are mutually exclusive".into(),
            ));
        }
        if !direct {
            return Err(CliError::Usage(
                "--credential-type cookie requires --from-file <PATH> or --from-stdin".into(),
            ));
        }
        let label = args
            .label
            .clone()
            .ok_or_else(|| CliError::Usage("--credential-type cookie requires --label".into()))?;
        validate_label_cli(&label)?;

        let bytes = read_secret_bytes(&args)?;
        validate_cookie_blob_json(&bytes)?;

        // Resolve session id. Operator may pass --session explicitly;
        // otherwise call vault.get_session_context for the most recent
        // active session.
        let session_id = match args.session.clone() {
            Some(s) => s,
            None => {
                let ctx = rpc
                    .call("vault.get_session_context", serde_json::Value::Null)
                    .await?;
                ctx.get("session_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        CliError::Usage(
                            "vault.get_session_context returned no session_id; \
                             pass --session <SESSION_ID> explicitly or run \
                             `loom session create` first"
                                .to_string(),
                        )
                    })?
                    .to_string()
            }
        };

        // Store the cookie blob bytes via vault.set_secret.
        let secret_hex = hex::encode(&bytes);
        drop(bytes); // D7: no shred-after-read on the operator-managed
                     // input file; just drop the in-memory copy here.
        let _set_resp = rpc
            .call(
                "vault.set_secret",
                serde_json::json!({
                    "label": label,
                    "secret_hex": secret_hex,
                    "overwrite": args.overwrite,
                    "session_id": session_id,
                }),
            )
            .await?;

        // Issue a Cookie-typed grant. origin is "*" because cookie
        // grants are scoped by the cookies' own `domain` fields, not
        // by a single origin URL like OAuth. scopes empty for the same
        // reason. ttl defaults to 30 days; the operator can revoke
        // earlier via `loom vault delete <label>` (cascades grants).
        let grant_resp = rpc
            .call(
                "vault.grant",
                serde_json::json!({
                    "session_id": session_id,
                    "origin": "*",
                    "scopes": [],
                    "ttl_seconds": 86_400u64 * 30,
                    "label": label,
                    "credential_type": "cookie",
                }),
            )
            .await?;
        emit_to_stdout("vault.grant", &grant_resp, cfg, None)?;
        return Ok(());
    }

    if direct {
        if args.provider.is_some() {
            return Err(CliError::Usage(
                "`vault add <provider>` and --from-stdin / --from-file are mutually exclusive"
                    .into(),
            ));
        }
        let label = args.label.clone().ok_or_else(|| {
            CliError::Usage("direct credential injection requires --label".into())
        })?;
        validate_label_cli(&label)?;
        let bytes = read_secret_bytes(&args)?;
        let secret_hex = hex::encode(&bytes);
        drop(bytes);

        let resp = rpc
            .call(
                "vault.set_secret",
                serde_json::json!({
                    "label": label,
                    "secret_hex": secret_hex,
                    "overwrite": args.overwrite,
                    "session_id": args.session,
                }),
            )
            .await?;
        emit_to_stdout("vault.set_secret", &resp, cfg, None)?;
        return Ok(());
    }

    // Legacy OAuth flow.
    let provider = args.provider.clone().ok_or_else(|| {
        CliError::Usage(
            "`vault add` requires a positional <provider> (e.g. `vault add github`) \
             OR --from-stdin / --from-file with --label"
                .into(),
        )
    })?;
    let resp = rpc
        .call(
            "vault.add",
            serde_json::json!({
                "provider": provider,
                "label": args.label,
                "yes": args.yes,
            }),
        )
        .await?;
    emit_to_stdout("vault.add", &resp, cfg, None)?;
    Ok(())
}

pub async fn delete(
    rpc: &RpcClient,
    cfg: &CliConfig,
    args: VaultDeleteArgs,
) -> Result<(), CliError> {
    validate_label_cli(&args.label)?;
    let resp = rpc
        .call(
            "vault.delete_secret",
            serde_json::json!({
                "label": args.label,
                "force": args.force,
                "session_id": args.session,
            }),
        )
        .await?;
    emit_to_stdout("vault.delete_secret", &resp, cfg, None)?;
    Ok(())
}

pub async fn list_labels(
    rpc: &RpcClient,
    cfg: &CliConfig,
    args: VaultListLabelsArgs,
) -> Result<(), CliError> {
    let resp = rpc
        .call(
            "vault.list_labels",
            serde_json::json!({"session_id": args.session}),
        )
        .await?;
    emit_to_stdout("vault.list_labels", &resp, cfg, None)?;
    Ok(())
}

pub async fn diagnose(
    rpc: &RpcClient,
    cfg: &CliConfig,
    _args: VaultDiagnoseArgs,
) -> Result<(), CliError> {
    let resp = rpc.call("vault.diagnose", serde_json::Value::Null).await?;
    emit_to_stdout("vault.diagnose", &resp, cfg, None)?;
    Ok(())
}

/// Subcommand → RPC mapping table — `interface_tests` asserts coverage.
pub const SUBCOMMAND_RPC_MAP: &[(&str, &str)] = &[
    ("grant", "vault.grant"),
    ("revoke", "vault.revoke"),
    ("list", "vault.list_grants"),
    ("add", "vault.add"),
    ("add-direct", "vault.set_secret"),
    ("delete", "vault.delete_secret"),
    ("list-labels", "vault.list_labels"),
    ("diagnose", "vault.diagnose"),
];
