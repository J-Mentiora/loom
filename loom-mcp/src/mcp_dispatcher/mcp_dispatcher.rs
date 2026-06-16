// McpDispatcher — routes MCP methods to handlers.
// Types only. Implementation in mod.rs.

use crate::mcp_observability::McpObservability;
use crate::resource_tracker::ResourceTracker;
use crate::rpc_client::RpcClient;
use crate::tool_cache::ToolCache;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Server capabilities advertised in `initialize`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServerCapabilities {
    pub tools: ToolsCapability,
    pub resources: ResourcesCapability,
    pub prompts: PromptsCapability,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolsCapability {
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourcesCapability {
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
    pub subscribe: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptsCapability {
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

/// Result body of `initialize`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

/// Params of `tools/call`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsCallParams {
    pub name: String,
    #[serde(default)]
    pub arguments: serde_json::Value,
}

/// Params of `resources/read`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourcesReadParams {
    pub uri: String,
}

/// Env var: optional `u64` seed forwarded to the implicit session's
/// `session.create` (cross-run determinism).
pub const ENV_SESSION_SEED: &str = "LOOM_MCP_SESSION_SEED";
/// Env var: optional fixed Unix epoch (ms) forwarded as `clock_anchor`
/// so the injected browser clock is pinned across runs.
pub const ENV_SESSION_CLOCK_ANCHOR: &str = "LOOM_MCP_SESSION_CLOCK_ANCHOR";
/// Env var: optional profile override for the implicit session.
pub const ENV_SESSION_PROFILE: &str = "LOOM_MCP_SESSION_PROFILE";
/// Env var: optional JSON object of budget limits forwarded as `budget`
/// to `session.create` (e.g. `{"wall_clock":"30s"}` or
/// `{"session_walltime_ms":30000}`). Parsed as opaque JSON here; the
/// daemon owns `BudgetLimits` deserialization + key-allowlisting.
pub const ENV_SESSION_BUDGET: &str = "LOOM_MCP_SESSION_BUDGET";

/// Profile used for the implicit session when no override is configured.
/// `standard` so denylist surprises don't confuse MCP clients (the
/// safe-profile evaluate denylist would bounce `window.location` and
/// friends; that's a CLI safety preset, not an MCP-default expectation).
pub const IMPLICIT_SESSION_PROFILE: &str = "standard";

/// Creation options for the implicit session. The env baseline is read
/// once at startup (`SessionOptions::from_env`); `loom.session.reset`
/// can override per-reset. `None` fields are omitted from the
/// `session.create` wire params, so the all-default shape stays
/// byte-identical to the pre-feature `{"profile":"standard"}`.
///
/// `Eq` is intentionally NOT derived: `budget` is opaque `serde_json::Value`
/// (forwarded to the daemon, which owns `BudgetLimits`), and `Value` is only
/// `PartialEq` (it can hold `f64`). Nothing keys a map/set on `SessionOptions`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionOptions {
    pub seed: Option<u64>,
    pub clock_anchor: Option<u64>,
    pub profile: Option<String>,
    /// Opaque budget limits forwarded verbatim to `session.create`
    /// (`{"wall_clock":"30s"}` or `{"session_walltime_ms":30000}`).
    pub budget: Option<serde_json::Value>,
}

/// Live implicit-session record: the daemon-assigned id plus the exact
/// options it was created with (self-heal recreates with these) and the
/// daemon-reported creation timestamp (surfaced by `loom.session.info`).
#[derive(Debug, Clone)]
pub struct ImplicitSession {
    pub id: String,
    pub created_at_ms: u64,
    pub options: SessionOptions,
}

/// Mutex-guarded implicit-session state. One lock covers both the live
/// session and the options for the next `session.create`, so a reset
/// (close + recreate) is atomic w.r.t. concurrent tool calls.
#[derive(Debug, Default)]
pub struct ImplicitSessionState {
    /// The live session, if one has been created.
    pub session: Option<ImplicitSession>,
    /// Options for the NEXT `session.create` (env baseline at startup).
    pub options: SessionOptions,
}

/// MCP names of the server-local session tools. Unlike the `web.*`
/// family these are NOT derived from the daemon's `rpc.schemas`
/// registry (session.* RPCs are BUILTIN_CORE_METHODS without schemas) —
/// the MCP server defines their schemas itself and proxies to the
/// corresponding session.* RPCs.
pub const TOOL_SESSION_RESET: &str = "loom.session.reset";
pub const TOOL_SESSION_INFO: &str = "loom.session.info";
pub const TOOL_SESSION_DIFF: &str = "loom.session.diff";
pub const TOOL_SESSION_VALIDATE: &str = "loom.session.validate";
pub const TOOL_SESSION_EXPORT: &str = "loom.session.export";

/// Arguments of `loom.session.reset`. Absent fields fall back to the
/// env baseline (NOT a previous reset's overrides), so each reset is
/// hermetic: its outcome depends only on the environment + this call.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionResetParams {
    pub seed: Option<u64>,
    pub clock_anchor: Option<u64>,
    pub profile: Option<String>,
    /// Opaque budget limits; merged over the env baseline like the other
    /// knobs and forwarded to `session.create` (daemon validates).
    pub budget: Option<serde_json::Value>,
}

/// Arguments of `loom.session.diff`.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionDiffParams {
    /// The session to diff the current implicit session against
    /// (typically a previous run's id from `loom.session.info`/`reset`).
    pub other_session_id: String,
    #[serde(default)]
    pub include_screenshots: bool,
    #[serde(default)]
    pub show_dom_diffs: bool,
}

/// Arguments of `loom.session.validate`.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionValidateParams {
    /// Session to validate; defaults to the current implicit session.
    pub session_id: Option<String>,
}

/// Arguments of `loom.session.export`.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionExportParams {
    /// Session to export; defaults to the current implicit session.
    pub session_id: Option<String>,
    /// Export format; defaults to `json` (the daemon default).
    pub format: Option<String>,
}

pub const METHOD_INITIALIZE: &str = "initialize";
pub const METHOD_SHUTDOWN: &str = "shutdown";
pub const METHOD_PING: &str = "ping";
pub const METHOD_TOOLS_LIST: &str = "tools/list";
pub const METHOD_TOOLS_CALL: &str = "tools/call";
pub const METHOD_RESOURCES_LIST: &str = "resources/list";
pub const METHOD_RESOURCES_READ: &str = "resources/read";
pub const METHOD_PROMPTS_LIST: &str = "prompts/list";
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// Concrete dispatcher.
pub struct McpDispatcher {
    pub(crate) tool_cache: Arc<ToolCache>,
    pub(crate) resource_tracker: Arc<ResourceTracker>,
    pub(crate) rpc: Arc<RpcClient>,
    pub(crate) obs: Arc<McpObservability>,
    /// Cancelled when shutdown is requested — by the MCP `shutdown`
    /// method or the process signal task. `mcp_main::serve_until_shutdown`
    /// selects on it against the stdio loop, so cancelling actually
    /// terminates the server (its `AtomicBool` predecessor was stored
    /// into but never read by anything).
    pub(crate) shutdown: tokio_util::sync::CancellationToken,
    /// Implicit session auto-created on first tool call and reused
    /// across the MCP server's lifetime. Lets MCP clients call
    /// `loom.web.*` tools without first having to call session.create
    /// (which isn't even in the tool list because `rpc.schemas` only
    /// returns schema-driven methods, not the BUILTIN_CORE_METHODS
    /// that session.* belongs to). Closed on shutdown. Carries the
    /// creation options so an idle-evicted session can be transparently
    /// recreated with the same determinism knobs (self-heal).
    pub(crate) implicit_session: Arc<tokio::sync::Mutex<ImplicitSessionState>>,
    /// Env-derived options snapshot taken at startup. `loom.session.reset`
    /// merges its explicit arguments over THIS baseline (never over a
    /// previous reset's overrides) so every reset is hermetic.
    pub(crate) baseline_options: SessionOptions,
}
