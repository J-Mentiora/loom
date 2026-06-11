// SessionCommands — handlers for `loom session.*` subcommands.
//
// # Contract semantics
// - **Routing.** Each handler maps to exactly one RPC method per
//   the subcommand → RPC table:
//   `create→session.create`, `inspect→session.inspect`,
//   `list→session.list`, `close→session.close`,
//   `abort→session.abort`, `replay→session.replay`,
//   `diff→session.diff`, `export→session.export`,
//   `validate→session.validate`.
// - **Receipt pass-through.** Handlers forward the
//   `serde_json::Value` receipt to `OutputFormatter::write` verbatim.
//   No field rewriting, no prose augmentation. Clippy lint forbids
//   `Receipt::redact` calls.
// - **Exit codes.** Handlers return `Result<(), CliError>`;
//   exit-code mapping is owned exclusively by `ErrorMapper`.

use clap::{Args, ValueEnum};
use loom_core::budget_enforcer::BudgetLimits;
use serde::{Deserialize, Serialize};

use crate::cli_config::CliConfig;
use crate::output_formatter::emit_to_stdout;
use crate::rpc_client::RpcClient;
use crate::CliError;

/// `--capture-policy` CLI value. Wire form is the
/// lowercased variant ("minimal" / "default" / "full"). Clap rejects
/// unknown values with `ErrorKind::InvalidValue` (exit 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[clap(rename_all = "lowercase")]
pub enum CapturePolicyArg {
    Minimal,
    Default,
    Full,
}

impl CapturePolicyArg {
    /// Wire form for the JSON-RPC `session.create` request param.
    pub fn as_wire_str(self) -> &'static str {
        match self {
            CapturePolicyArg::Minimal => "minimal",
            CapturePolicyArg::Default => "default",
            CapturePolicyArg::Full => "full",
        }
    }
}

/// `loom session create` arguments. Flag names mirror the
/// `session.create` RPC request schema.
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct CreateArgs {
    /// Profile name. One of: `safe` (default; denylists destructive
    /// evaluate patterns + confines downloads), `standard` (no
    /// evaluate denylist; default download dir), or `full` (no
    /// guards). Omitting the flag uses `safe`.
    #[arg(long)]
    pub profile: Option<String>,
    /// Network mode. `live` (the default) is the only mode: page
    /// traffic is always fetched live and response bodies are never
    /// recorded, so there is no page-network record/replay to select.
    /// Any other value is rejected with `invalid_network_mode`.
    #[arg(long = "network-mode")]
    pub network_mode: Option<String>,
    /// Determinism seed (replay anchor).
    #[arg(long)]
    pub seed: Option<u64>,
    /// Pin the page clock to a fixed time so repeat recordings match.
    #[arg(
        long = "clock-anchor",
        long_help = "Pin the page clock to a fixed time so repeat recordings match.\n\n\
            Value is Unix epoch milliseconds (e.g. 1700000000000 = 2023-11-14). Pass the \
            same value across runs, together with --seed, to get an identical `loom session \
            diff` (field_diffs=0). Without it, the wall-clock time leaks into the recording \
            via Date.now()/performance.now() and two runs differ. Composes with --seed; \
            works without it (the default seed already pins RNG). No effect under \
            --no-determinism."
    )]
    pub clock_anchor: Option<u64>,
    /// Budget overrides, comma-separated key=value pairs.
    /// Keys: network=NMB, wall_clock=Ns, dom_nodes=N, js_heap=NMB.
    /// Example: --budget network=10MB,wall_clock=30s
    #[arg(long)]
    #[serde(default)]
    pub budget: Option<String>,
    /// Capture policy. One of `minimal`, `default`, `full`.
    /// Controls receipt tier-2/tier-3 field emission.
    #[arg(long = "capture-policy", value_enum)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_policy: Option<CapturePolicyArg>,
    /// Disable the default analytics/ads/telemetry blocklist for this
    /// session. Escape hatch for testing
    /// against pages that legitimately depend on the blocked services.
    /// When omitted, the default blocklist enforces sub-resource gating.
    #[arg(long = "no-blocklist", default_value_t = false)]
    #[serde(default)]
    pub no_blocklist: bool,

    /// Disable determinism for this session (settle-capture). By default loom
    /// freezes `Date.now`/animations and seeds `Math.random` so captures are
    /// byte-reproducible. With `--no-determinism` the page keeps real
    /// wall-clock + unseeded RNG (for live/non-reproducible capture). Such a
    /// session is recorded as NON-REPLAYABLE — `loom replay` refuses it.
    #[arg(long = "no-determinism", default_value_t = false)]
    #[serde(default)]
    pub no_determinism: bool,
}

/// Parse a --budget flag string into BudgetLimits.
/// Format: `key=value[,key=value...]`
/// - `network=NMB`    → network_bytes = N * 1024 * 1024
/// - `wall_clock=Ns`  → session_walltime_ms = N * 1000
/// - `dom_nodes=N`    → dom_nodes = N
/// - `js_heap=NMB`    → js_heap_bytes = N * 1024 * 1024
pub fn parse_budget_string(s: &str) -> Result<BudgetLimits, String> {
    let mut limits = BudgetLimits::default();
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (k, v) = part
            .split_once('=')
            .ok_or_else(|| format!("invalid budget segment (expected key=value): {part}"))?;
        match k.trim() {
            "network" => {
                limits.network_bytes =
                    parse_mb(v.trim()).ok_or_else(|| format!("invalid network value: {v}"))?;
            }
            "wall_clock" => {
                limits.session_walltime_ms =
                    parse_secs(v.trim()).ok_or_else(|| format!("invalid wall_clock value: {v}"))?;
            }
            "dom_nodes" => {
                limits.dom_nodes = v
                    .trim()
                    .parse::<u64>()
                    .map_err(|_| format!("invalid dom_nodes value: {v}"))?;
            }
            "js_heap" => {
                limits.js_heap_bytes =
                    parse_mb(v.trim()).ok_or_else(|| format!("invalid js_heap value: {v}"))?;
            }
            other => return Err(format!("unknown budget key: {other}")),
        }
    }
    Ok(limits)
}

/// Parse "NMB" → N * 1024 * 1024 (bytes). E.g. "10MB" → 10_485_760.
/// Refuses 0: BudgetLimits internally treats 0 as "unlimited", which is
/// the opposite of what an operator passing `--budget network=0` would
/// expect. Reject at parse time so the user gets a clear error rather
/// than a silently-permissive session.
fn parse_mb(s: &str) -> Option<u64> {
    let n: u64 = s.trim_end_matches("MB").parse().ok()?;
    if n == 0 {
        return None;
    }
    Some(n * 1024 * 1024)
}

/// Parse "Ns" → N * 1000 (ms). E.g. "30s" → 30_000.
/// Refuses 0 for the same reason as parse_mb (0 = unlimited internally,
/// confusing for operators).
fn parse_secs(s: &str) -> Option<u64> {
    let n: u64 = s.trim_end_matches('s').parse().ok()?;
    if n == 0 {
        return None;
    }
    Some(n * 1000)
}

/// Pre-validate budget keys against
/// `KNOWN_BUDGET_KEYS` from loom-core's `profile_registry`. Synthesizes
/// a `CliError::Receipt` envelope shape-equivalent to what the server
/// would emit (`code = invalid_budget_key`, `details.provided`,
/// `details.available`), so the CLI exits 1 with a typed receipt
/// instead of exit 2 (Usage) on unrecognized keys.
fn check_budget_keys(budget_str: &str) -> Result<(), CliError> {
    use loom_core::profile_registry::profile_registry::{is_known_budget_key, KNOWN_BUDGET_KEYS};
    for part in budget_str.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let key = match part.split_once('=') {
            Some((k, _)) => k.trim(),
            None => continue, // shape errors fall through to parse_budget_string → CliError::Usage
        };
        if !is_known_budget_key(key) {
            return Err(CliError::Receipt(serde_json::json!({
                "status": "error",
                "code": "invalid_budget_key",
                "message": format!("unknown budget key: {key}"),
                "details": {
                    "provided": key,
                    "available": KNOWN_BUDGET_KEYS,
                },
            })));
        }
    }
    Ok(())
}

/// `loom session inspect <id>` arguments.
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct InspectArgs {
    pub session_id: String,
    #[arg(long = "at-action")]
    pub at_action: Option<u64>,
}

/// `loom session list` arguments. Empty body — flags are global.
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct ListArgs {}

/// `loom session close <id>` arguments.
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct CloseArgs {
    pub session_id: String,
}

/// `loom session abort <id>` arguments.
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct AbortArgs {
    pub session_id: String,
    #[arg(long)]
    pub reason: Option<String>,
}

/// Parsed `--speed` value for `loom session replay`. The daemon's
/// `session.replay` wire contract is a JSON *number* (the SDKs send e.g.
/// `1.0`), so the documented CLI string forms are parsed at the clap
/// boundary and mapped via [`ReplaySpeed::as_wire_f64`]. A `String`
/// forwarded verbatim (the previous shape) never parsed daemon-side and
/// was silently dropped.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplaySpeed {
    /// Source pacing (wire value `1`).
    Realtime,
    /// `Nx` multiplier, e.g. `2x` or `1.5x` (wire value `N`).
    Multiplier(f64),
    /// Unpaced — as fast as the replay engine can drive (wire sentinel
    /// `0`, consistent with the workspace's 0-means-unlimited budget
    /// convention; the daemon accepts it explicitly).
    Max,
}

impl ReplaySpeed {
    /// Numeric wire form for the `session.replay` RPC `speed` param.
    pub fn as_wire_f64(self) -> f64 {
        match self {
            ReplaySpeed::Realtime => 1.0,
            ReplaySpeed::Multiplier(n) => n,
            ReplaySpeed::Max => 0.0,
        }
    }
}

/// clap value parser for `--speed`. Accepts the documented forms
/// `realtime`, `max`, and `Nx` (N > 0, finite; fractional allowed,
/// e.g. `1.5x`). Anything else is a clap `InvalidValue` (exit 2) with
/// the accepted forms in the message — no more silent discard.
pub fn parse_replay_speed(s: &str) -> Result<ReplaySpeed, String> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("realtime") {
        return Ok(ReplaySpeed::Realtime);
    }
    if s.eq_ignore_ascii_case("max") {
        return Ok(ReplaySpeed::Max);
    }
    let err = || format!("invalid speed {s:?}; expected `Nx` (e.g. `2x`), `max`, or `realtime`");
    let n = s
        .strip_suffix(['x', 'X'])
        .ok_or_else(err)?
        .parse::<f64>()
        .map_err(|_| err())?;
    if !n.is_finite() || n <= 0.0 {
        return Err(err());
    }
    Ok(ReplaySpeed::Multiplier(n))
}

/// `loom session replay <id>` arguments.
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct ReplayArgs {
    pub session_id: String,
    /// `Nx` (e.g. `2x`, `1.5x`), `max`, or `realtime`.
    #[arg(long, default_value = "realtime", value_parser = parse_replay_speed)]
    pub speed: ReplaySpeed,
}

/// `loom session diff <a> <b>` arguments.
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct DiffArgs {
    pub a: String,
    pub b: String,
    #[arg(long = "include-screenshots")]
    pub include_screenshots: bool,
    #[arg(long = "show-dom-diffs")]
    pub show_dom_diffs: bool,
}

/// `--format` for `loom session export`. Clap rejects unknown values with
/// `ErrorKind::InvalidValue` (exit 2) before any RPC call — closes the
/// "format=banana silently forwarded to daemon" leak.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[clap(rename_all = "lowercase")]
pub enum ExportFormat {
    Json,
    Tarball,
    Har,
    Cdp,
}

impl ExportFormat {
    /// Wire form for the JSON-RPC `session.export` request param.
    pub fn as_wire_str(self) -> &'static str {
        match self {
            ExportFormat::Json => "json",
            ExportFormat::Tarball => "tarball",
            ExportFormat::Har => "har",
            ExportFormat::Cdp => "cdp",
        }
    }
}

/// `loom session export <id> --format ...` arguments.
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct ExportArgs {
    pub session_id: String,
    /// `json`, `tarball`, `har`, or `cdp`.
    #[arg(long, value_enum)]
    pub format: ExportFormat,
    #[arg(long)]
    pub output: Option<std::path::PathBuf>,
}

/// `loom session validate <id>` arguments.
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct ValidateArgs {
    pub session_id: String,
}

/// `loom session reap [--apply]` arguments.
#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct ReapArgs {
    /// Actually quarantine the corrupt orphans. Without this flag, reap only
    /// previews (dry-run) — nothing on disk is moved.
    #[arg(long)]
    pub apply: bool,
}

/// One handler per subcommand. Each calls exactly one RPC method on
/// `RpcClient`, then forwards the raw receipt to `OutputFormatter`.
pub async fn create(rpc: &RpcClient, cfg: &CliConfig, args: CreateArgs) -> Result<(), CliError> {
    let _ = cfg;
    let mut params = serde_json::Map::new();
    if let Some(profile) = &args.profile {
        params.insert(
            "profile".to_string(),
            serde_json::Value::String(profile.clone()),
        );
    }
    if let Some(nm) = &args.network_mode {
        params.insert(
            "network_mode".to_string(),
            serde_json::Value::String(nm.clone()),
        );
    }
    if let Some(seed) = args.seed {
        params.insert("seed".to_string(), serde_json::Value::Number(seed.into()));
    }
    if let Some(clock_anchor) = args.clock_anchor {
        params.insert(
            "clock_anchor".to_string(),
            serde_json::Value::Number(clock_anchor.into()),
        );
    }
    if let Some(budget_str) = &args.budget {
        // synthesize a typed `invalid_budget_key` receipt
        // (status="error", exit 1) instead of CliError::Usage (exit 2)
        // when the user supplies an unrecognized budget key. Keeps CLI
        // and server-rejected envelopes shape-equivalent so downstream
        // tooling (jq filters, dashboards) can match on `code`.
        check_budget_keys(budget_str)?;
        let limits = parse_budget_string(budget_str).map_err(CliError::Usage)?;
        params.insert(
            "budget".to_string(),
            serde_json::to_value(limits).map_err(|e| CliError::Internal(e.to_string()))?,
        );
    }
    if let Some(cp) = args.capture_policy {
        // forward the CLI choice as the `capture_policy`
        // wire field; server validation (session_validation) rejects
        // unknown strings via `InvalidCapturePolicy` even though clap
        // already constrained the CLI value space.
        params.insert(
            "capture_policy".to_string(),
            serde_json::Value::String(cp.as_wire_str().to_string()),
        );
    }
    // forward `--no-blocklist` as a boolean wire field.
    // Only insert when true so default-disabled (no flag) keeps params
    // shape consistent with pre-feature payloads.
    if args.no_blocklist {
        params.insert("no_blocklist".to_string(), serde_json::Value::Bool(true));
    }
    // forward `--no-determinism` (settle-capture 4b); only when true so the
    // default deterministic session keeps the pre-feature params shape.
    if args.no_determinism {
        params.insert("no_determinism".to_string(), serde_json::Value::Bool(true));
    }
    let resp = rpc
        .call("session.create", serde_json::Value::Object(params))
        .await?;
    emit_to_stdout("session.create", &resp, cfg, None)?;
    crate::error_mapper::receipt_to_result(resp).map(|_| ())
}

pub async fn inspect(rpc: &RpcClient, cfg: &CliConfig, args: InspectArgs) -> Result<(), CliError> {
    let resp = rpc
        .call(
            "session.inspect",
            serde_json::json!({
                "session_id": args.session_id,
                "at_action": args.at_action,
            }),
        )
        .await?;

    #[derive(serde::Deserialize)]
    struct SessionInspection {
        manifest_summary: serde_json::Value,
    }
    let inspection: SessionInspection = serde_json::from_value(resp)
        .map_err(|e| CliError::Internal(format!("inspect response parse: {e}")))?;

    emit_to_stdout("session.inspect", &inspection.manifest_summary, cfg, None)?;
    Ok(())
}

pub async fn list(rpc: &RpcClient, cfg: &CliConfig, args: ListArgs) -> Result<(), CliError> {
    let _ = args;
    let resp = rpc.call("session.list", serde_json::json!({})).await?;
    emit_to_stdout("session.list", &resp, cfg, None)?;
    crate::error_mapper::receipt_to_result(resp).map(|_| ())
}

pub async fn close(rpc: &RpcClient, cfg: &CliConfig, args: CloseArgs) -> Result<(), CliError> {
    let resp = rpc
        .call(
            "session.close",
            serde_json::json!({ "session_id": args.session_id }),
        )
        .await?;
    emit_to_stdout("session.close", &resp, cfg, None)?;
    // close on already-closed session returns a result-body
    // receipt with status="error"; raise to exit 1.
    crate::error_mapper::receipt_to_result(resp).map(|_| ())
}

pub async fn abort(rpc: &RpcClient, cfg: &CliConfig, args: AbortArgs) -> Result<(), CliError> {
    let resp = rpc
        .call(
            "session.abort",
            serde_json::json!({
                "session_id": args.session_id,
                "reason": args.reason,
            }),
        )
        .await?;
    emit_to_stdout("session.abort", &resp, cfg, None)?;
    crate::error_mapper::receipt_to_result(resp).map(|_| ())
}

pub async fn replay(rpc: &RpcClient, cfg: &CliConfig, args: ReplayArgs) -> Result<(), CliError> {
    let resp = rpc
        .call(
            "session.replay",
            serde_json::json!({
                "session_id": args.session_id,
                // Numeric per the daemon contract (request_router parses
                // `as_f64`; the SDKs send numbers). See `ReplaySpeed`.
                "speed": args.speed.as_wire_f64(),
            }),
        )
        .await?;

    // replay session is terminal once the action chain
    // has been re-driven. Close it explicitly on the daemon side so any
    // subsequent action.* call against the new session_id is rejected
    // by `close-is-not-terminal` enforcement (consistent with the contract
    // that replays produce an artifact, not a live session).
    if let Some(new_session_id) = resp.get("session_id").and_then(|v| v.as_str()) {
        let _ = rpc
            .call(
                "session.close",
                serde_json::json!({ "session_id": new_session_id }),
            )
            .await;
    }

    emit_to_stdout("session.replay", &resp, cfg, None)?;
    Ok(())
}

pub async fn diff(rpc: &RpcClient, cfg: &CliConfig, args: DiffArgs) -> Result<(), CliError> {
    let resp = rpc
        .call(
            "session.diff",
            serde_json::json!({
                "a": args.a,
                "b": args.b,
                "include_screenshots": args.include_screenshots,
                "show_dom_diffs": args.show_dom_diffs,
            }),
        )
        .await?;

    #[derive(serde::Deserialize)]
    struct DiffReport {
        diff: serde_json::Value,
    }
    let report: DiffReport = serde_json::from_value(resp)
        .map_err(|e| CliError::Internal(format!("diff response parse: {e}")))?;

    emit_to_stdout("session.diff", &report.diff, cfg, None)?;

    // Exit 6 (CliError::SessionsDiffer) if there are any structural differences.
    // SessionsDiffer is a dedicated variant — distinct from Internal (CLI bug, exit 2)
    // and from Receipt (daemon error, exit 1).
    let field_diff_count = report
        .diff
        .get("field_diffs")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let action_count_delta = report
        .diff
        .get("action_count_delta")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    if field_diff_count > 0 || action_count_delta != 0 {
        return Err(CliError::SessionsDiffer(format!(
            "{field_diff_count} field diffs, action_count_delta={action_count_delta}"
        )));
    }
    Ok(())
}

pub async fn export(rpc: &RpcClient, _cfg: &CliConfig, args: ExportArgs) -> Result<(), CliError> {
    // 1. Call session.export → ExportInfo { artifact_ref }
    let resp = rpc
        .call(
            "session.export",
            serde_json::json!({
                "session_id": args.session_id,
                "format": args.format.as_wire_str(),
            }),
        )
        .await?;

    #[derive(serde::Deserialize)]
    struct ExportInfo {
        artifact_ref: String,
    }
    let export_info: ExportInfo = serde_json::from_value(resp)
        .map_err(|e| CliError::Internal(format!("export response parse: {e}")))?;

    // 2. Call content.get → ContentData { data_hex }
    let content_resp = rpc
        .call(
            "content.get",
            serde_json::json!({ "artifact_ref": export_info.artifact_ref }),
        )
        .await?;

    #[derive(serde::Deserialize)]
    struct ContentData {
        data_hex: String,
    }
    let content: ContentData = serde_json::from_value(content_resp)
        .map_err(|e| CliError::Internal(format!("content response parse: {e}")))?;

    // 3. Hex-decode to raw bytes
    let bytes = hex::decode(&content.data_hex)
        .map_err(|e| CliError::Internal(format!("hex decode: {e}")))?;

    // 4. Write to --output path or stdout
    use std::io::Write as _;
    if let Some(path) = args.output {
        std::fs::write(&path, &bytes).map_err(|e| CliError::Internal(e.to_string()))?;
    } else {
        std::io::stdout()
            .lock()
            .write_all(&bytes)
            .map_err(|e| CliError::Internal(e.to_string()))?;
    }
    Ok(())
}

pub async fn validate(
    rpc: &RpcClient,
    _cfg: &CliConfig,
    args: ValidateArgs,
) -> Result<(), CliError> {
    let resp = rpc
        .call(
            "session.validate",
            serde_json::json!({ "session_id": args.session_id }),
        )
        .await?;

    #[derive(serde::Deserialize)]
    struct ValidationResult {
        passed: bool,
        reasons: Vec<String>,
    }
    let result: ValidationResult = serde_json::from_value(resp)
        .map_err(|e| CliError::Internal(format!("validate response parse: {e}")))?;

    if result.passed {
        println!("PASS");
    } else {
        println!("FAIL");
        for reason in &result.reasons {
            println!("  - {reason}");
        }
        // validation failures (e.g. tampered envelope MAC)
        // belong in the receipt-error class (exit 1), not the internal-bug
        // class (exit 2). Construct a synthetic receipt so Display matches
        // the action-error format ("Error: <code>: <message>").
        return Err(CliError::Receipt(serde_json::json!({
            "status": "error",
            "code": "session-validation-failed",
            "message": "session validation failed",
            "details": { "reasons": result.reasons },
        })));
    }
    Ok(())
}

pub async fn reap(rpc: &RpcClient, _cfg: &CliConfig, args: ReapArgs) -> Result<(), CliError> {
    let dry_run = !args.apply;
    let resp = rpc
        .call("session.reap", serde_json::json!({ "dry_run": dry_run }))
        .await?;

    #[derive(serde::Deserialize)]
    struct ReapResult {
        quarantined: Vec<String>,
        skipped_live: u64,
        dry_run: bool,
        quarantine_dir: Option<String>,
        failed: Vec<String>,
        #[serde(default)]
        idle_evicted: Vec<String>,
        #[serde(default)]
        zombies_closed: Vec<String>,
        #[serde(default)]
        orphan_browsers_killed: Vec<String>,
        #[serde(default)]
        orphan_dirs_removed: u64,
    }
    let result: ReapResult = serde_json::from_value(resp)
        .map_err(|e| CliError::Internal(format!("reap response parse: {e}")))?;

    let verb = if result.dry_run {
        "would reap"
    } else {
        "reaped"
    };
    println!(
        "{verb}: {} idle session(s), {} zombie session(s), {} orphan browser tree(s), \
         {} corrupt orphan session(s); {} live session(s) skipped",
        result.idle_evicted.len(),
        result.zombies_closed.len(),
        result.orphan_browsers_killed.len(),
        result.quarantined.len(),
        result.skipped_live,
    );
    let print_ids = |label: &str, ids: &[String]| {
        if !ids.is_empty() {
            println!("  {label}:");
            for id in ids {
                println!("    - {id}");
            }
        }
    };
    print_ids("idle", &result.idle_evicted);
    print_ids("zombie", &result.zombies_closed);
    print_ids("orphan-browser", &result.orphan_browsers_killed);
    print_ids("corrupt-orphan (quarantined)", &result.quarantined);
    if result.orphan_dirs_removed > 0 {
        println!("  orphan dirs removed: {}", result.orphan_dirs_removed);
    }
    if let Some(dir) = &result.quarantine_dir {
        if !result.quarantined.is_empty() {
            println!("quarantine dir: {dir}");
        }
    }
    for f in &result.failed {
        println!("  ! failed: {f}");
    }
    let nothing = result.idle_evicted.is_empty()
        && result.zombies_closed.is_empty()
        && result.orphan_browsers_killed.is_empty()
        && result.quarantined.is_empty();
    if result.dry_run && !nothing {
        println!("(dry-run — re-run with --apply to reap them)");
    }
    Ok(())
}

/// Compile-time mapping table — used by `interface_tests` to assert
/// subcommand coverage.
pub const SUBCOMMAND_RPC_MAP: &[(&str, &str)] = &[
    ("create", "session.create"),
    ("inspect", "session.inspect"),
    ("list", "session.list"),
    ("close", "session.close"),
    ("abort", "session.abort"),
    ("replay", "session.replay"),
    ("diff", "session.diff"),
    ("export", "session.export"),
    ("validate", "session.validate"),
    ("reap", "session.reap"),
];
