pub mod mcp_dispatcher;
pub use mcp_dispatcher::*;

#[cfg(test)]
mod interface_tests;

use crate::error_mapper::ErrorMapper;
use crate::stdio_transport::{McpProtocolError, McpRequest, McpResponse};
use loom_rpc::error::{LoomError, LoomErrorCode};
use std::sync::Arc;

impl SessionOptions {
    /// Read the implicit-session options from the process environment.
    /// Called once at server startup (`mcp_main::run`). A malformed
    /// numeric value is a hard startup error — silently dropping a
    /// determinism knob would hand the consumer non-deterministic
    /// captures that LOOK deterministic.
    pub fn from_env() -> Result<Self, LoomError> {
        Self::from_values(
            std::env::var(ENV_SESSION_SEED).ok(),
            std::env::var(ENV_SESSION_CLOCK_ANCHOR).ok(),
            std::env::var(ENV_SESSION_PROFILE).ok(),
            std::env::var(ENV_SESSION_BUDGET).ok(),
            std::env::var(ENV_SESSION_NO_DETERMINISM).ok(),
            std::env::var(ENV_SESSION_AUDIO).ok(),
        )
    }

    /// Parse raw env-shaped values. Split from `from_env` so tests can
    /// exercise parsing without mutating process-global env state.
    /// Empty/whitespace values are treated as unset (the shell `VAR=`
    /// form); the profile is passed through verbatim — the daemon's
    /// allowlist validation owns profile names.
    pub fn from_values(
        seed: Option<String>,
        clock_anchor: Option<String>,
        profile: Option<String>,
        budget: Option<String>,
        no_determinism: Option<String>,
        audio: Option<String>,
    ) -> Result<Self, LoomError> {
        Ok(Self {
            seed: parse_env_u64(ENV_SESSION_SEED, seed)?,
            clock_anchor: parse_env_u64(ENV_SESSION_CLOCK_ANCHOR, clock_anchor)?,
            profile: profile
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty()),
            budget: parse_env_json(ENV_SESSION_BUDGET, budget)?,
            no_determinism: parse_env_bool(ENV_SESSION_NO_DETERMINISM, no_determinism)?,
            audio: parse_env_bool(ENV_SESSION_AUDIO, audio)?,
        })
    }

    /// Build the `session.create` wire params. `None` fields are
    /// omitted entirely, so with all options unset this is exactly
    /// `{"profile":"standard"}` — byte-identical to the pre-feature
    /// shape (acceptance: no env vars set → no behavior change).
    pub fn to_create_params(&self) -> serde_json::Value {
        let mut params = serde_json::Map::new();
        params.insert(
            "profile".to_string(),
            serde_json::Value::String(
                self.profile
                    .clone()
                    .unwrap_or_else(|| IMPLICIT_SESSION_PROFILE.to_string()),
            ),
        );
        if let Some(seed) = self.seed {
            params.insert("seed".to_string(), serde_json::json!(seed));
        }
        if let Some(anchor) = self.clock_anchor {
            params.insert("clock_anchor".to_string(), serde_json::json!(anchor));
        }
        if let Some(budget) = &self.budget {
            params.insert("budget".to_string(), budget.clone());
        }
        // Only emit the key when enabling it, so the all-default shape stays
        // byte-identical to the pre-feature `{"profile":"standard"}`.
        if self.no_determinism == Some(true) {
            params.insert("no_determinism".to_string(), serde_json::json!(true));
        }
        // voice-call-io: only emit `audio` when enabling it, keeping the
        // all-default shape byte-identical to the pre-feature params.
        if self.audio == Some(true) {
            params.insert("audio".to_string(), serde_json::json!(true));
        }
        serde_json::Value::Object(params)
    }
}

fn parse_env_u64(var: &str, raw: Option<String>) -> Result<Option<u64>, LoomError> {
    let Some(raw) = raw else { return Ok(None) };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed.parse::<u64>().map(Some).map_err(|_| {
        LoomError::new(
            LoomErrorCode::InvalidArgument,
            format!("{var} must be a non-negative integer, got {trimmed:?}"),
        )
    })
}

/// Parse an env-shaped boolean (the no-determinism baseline). Empty/whitespace
/// is unset (shell `VAR=`). Truthy: `1`/`true`/`yes`/`on` (case-insensitive);
/// falsy: `0`/`false`/`no`/`off`. Anything else is a hard startup error — a
/// typo'd flag must not silently leave determinism on (it would hand the
/// consumer a frozen-clock session that LOOKS like it asked for live time).
fn parse_env_bool(var: &str, raw: Option<String>) -> Result<Option<bool>, LoomError> {
    let Some(raw) = raw else { return Ok(None) };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(Some(true)),
        "0" | "false" | "no" | "off" => Ok(Some(false)),
        _ => Err(LoomError::new(
            LoomErrorCode::InvalidArgument,
            format!("{var} must be a boolean (1/true/yes/on or 0/false/no/off), got {trimmed:?}"),
        )),
    }
}

/// Parse an env-shaped JSON value (the budget baseline). Empty/whitespace
/// is unset (shell `VAR=`); a malformed value is a hard startup error —
/// silently dropping the budget would let a runaway page outlive its
/// intended kill deadline while LOOKING budgeted. Shape is validated
/// daemon-side (`session_validation`), so this only checks it parses.
fn parse_env_json(var: &str, raw: Option<String>) -> Result<Option<serde_json::Value>, LoomError> {
    let Some(raw) = raw else { return Ok(None) };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    serde_json::from_str(trimmed).map(Some).map_err(|e| {
        LoomError::new(
            LoomErrorCode::InvalidArgument,
            format!("{var} must be valid JSON, got {trimmed:?}: {e}"),
        )
    })
}

impl McpDispatcher {
    pub fn new(
        tool_cache: Arc<crate::tool_cache::ToolCache>,
        resource_tracker: Arc<crate::resource_tracker::ResourceTracker>,
        rpc: Arc<crate::rpc_client::RpcClient>,
        obs: Arc<crate::mcp_observability::McpObservability>,
        shutdown: tokio_util::sync::CancellationToken,
        session_options: SessionOptions,
    ) -> Arc<Self> {
        Arc::new(Self {
            tool_cache,
            resource_tracker,
            rpc,
            obs,
            shutdown,
            implicit_session: Arc::new(tokio::sync::Mutex::new(ImplicitSessionState {
                session: None,
                options: session_options.clone(),
            })),
            baseline_options: session_options,
            recreated_count: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Look up or lazily create the implicit session that backs every
    /// `loom.web.*` tool call. Mutex'd so concurrent first-tool-call
    /// races serialize to a single session.create (one session per MCP
    /// server process, reused across the conversation).
    async fn ensure_implicit_session(self: &Arc<Self>) -> Result<String, LoomError> {
        let mut guard = self.implicit_session.lock().await;
        self.ensure_locked(&mut guard).await.map(|s| s.id)
    }

    /// Locked core of `ensure_implicit_session`, shared with the
    /// `loom.session.*` handlers that already hold the state lock.
    async fn ensure_locked(
        &self,
        guard: &mut ImplicitSessionState,
    ) -> Result<ImplicitSession, LoomError> {
        if let Some(s) = guard.session.as_ref() {
            return Ok(s.clone());
        }
        let created = self.create_implicit_session(guard.options.clone()).await?;
        guard.session = Some(created.clone());
        Ok(created)
    }

    /// One `session.create` round trip with `options` (env baseline by
    /// default; see `SessionOptions`).
    async fn create_implicit_session(
        &self,
        options: SessionOptions,
    ) -> Result<ImplicitSession, LoomError> {
        let resp = self
            .rpc
            .call("session.create", options.to_create_params())
            .await?;
        let id = resp
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                crate::error_mapper::ErrorMapper::from_schema_parse(
                    "session.create response missing session_id",
                )
            })?
            .to_string();
        let created_at_ms = resp
            .get("created_at_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        Ok(ImplicitSession {
            id,
            created_at_ms,
            options,
        })
    }

    /// True for the daemon error codes that mean "the implicit session no
    /// longer exists": idle-reaper eviction or a daemon restart
    /// (`session_not_found`), an out-of-band `loom session close`
    /// (`session_closed`), or a budget/operator abort (`session_aborted`).
    fn implicit_session_gone(code: LoomErrorCode) -> bool {
        matches!(
            code,
            LoomErrorCode::SessionNotFound
                | LoomErrorCode::SessionClosed
                | LoomErrorCode::SessionAborted
        )
    }

    /// Drop the cached implicit session iff it is still `stale_id` — a
    /// concurrent tool call may already have healed it, and discarding
    /// the replacement would trigger a second pointless recreate.
    async fn invalidate_implicit_session(&self, stale_id: &str) {
        let mut guard = self.implicit_session.lock().await;
        if guard.session.as_ref().is_some_and(|s| s.id == stale_id) {
            guard.session = None;
            // Count the gone-path recreation exactly once: only the call that
            // actually drops the live session bumps the counter (a concurrent
            // call that already healed leaves `session` mismatched and skips
            // this arm). `loom.session.reset` / `close` use `.take()` and
            // never reach here, so operator resets are excluded by design.
            self.recreated_count
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Close the implicit session if one was created. Best-effort; an
    /// error here means the daemon already cleaned the session up
    /// (e.g. operator hit `loom session close` out-of-band) and there's
    /// nothing to do.
    pub async fn close_implicit_session(self: &Arc<Self>) {
        let mut guard = self.implicit_session.lock().await;
        if let Some(s) = guard.session.take() {
            let _ = self
                .rpc
                .call("session.close", serde_json::json!({ "session_id": s.id }))
                .await;
        }
    }

    pub async fn dispatch(self: &Arc<Self>, req: McpRequest) -> Option<McpResponse> {
        // Notifications (no id) → no response.
        let id = req.id.clone()?;

        // Route to per-method handlers.
        let (result_val, error_val) = match req.method.as_str() {
            METHOD_INITIALIZE => {
                let v = serde_json::to_value(self.initialize().await)
                    .unwrap_or(serde_json::Value::Null);
                (Some(v), None)
            }
            METHOD_PING => (Some(self.ping().await), None),
            METHOD_TOOLS_LIST => {
                let tools = self.tools_list().await;
                let v = serde_json::json!({ "tools": tools });
                (Some(v), None)
            }
            METHOD_TOOLS_CALL => {
                let params: ToolsCallParams =
                    serde_json::from_value(req.params).unwrap_or(ToolsCallParams {
                        name: String::new(),
                        arguments: serde_json::Value::Null,
                    });
                let tr = self.tools_call(params).await;
                let v = serde_json::to_value(tr).unwrap_or(serde_json::Value::Null);
                (Some(v), None)
            }
            METHOD_RESOURCES_LIST => match self.resources_list().await {
                Ok(resources) => {
                    let v = serde_json::json!({ "resources": resources });
                    (Some(v), None)
                }
                Err(e) => {
                    let tr = ErrorMapper::to_tool_result(e);
                    let v = serde_json::to_value(tr).unwrap_or(serde_json::Value::Null);
                    (Some(v), None)
                }
            },
            METHOD_RESOURCES_READ => {
                let params: ResourcesReadParams = serde_json::from_value(req.params)
                    .unwrap_or(ResourcesReadParams { uri: String::new() });
                match self.resources_read(params).await {
                    Ok(contents) => {
                        // MCP 2024-11-05 defines the resources/read result as
                        // { "contents": [TextResourceContents | BlobResourceContents] }
                        // — strict clients reject a bare contents object.
                        let c = serde_json::to_value(contents).unwrap_or(serde_json::Value::Null);
                        let v = serde_json::json!({ "contents": [c] });
                        (Some(v), None)
                    }
                    Err(e) => {
                        let tr = ErrorMapper::to_tool_result(e);
                        let v = serde_json::to_value(tr).unwrap_or(serde_json::Value::Null);
                        (Some(v), None)
                    }
                }
            }
            METHOD_PROMPTS_LIST => {
                let v = serde_json::json!({ "prompts": self.prompts_list().await });
                (Some(v), None)
            }
            METHOD_SHUTDOWN => {
                self.shutdown();
                (Some(serde_json::json!({})), None)
            }
            _ => {
                let err = Self::unknown_method_error(&req.method);
                return Some(McpResponse {
                    jsonrpc: "2.0".into(),
                    id,
                    result: None,
                    error: Some(err),
                });
            }
        };

        Some(McpResponse {
            jsonrpc: "2.0".into(),
            id,
            result: result_val,
            error: error_val,
        })
    }

    pub async fn initialize(self: &Arc<Self>) -> InitializeResult {
        InitializeResult {
            protocol_version: MCP_PROTOCOL_VERSION.into(),
            capabilities: ServerCapabilities {
                tools: ToolsCapability {
                    list_changed: false,
                },
                resources: ResourcesCapability {
                    list_changed: false,
                    subscribe: false,
                },
                prompts: PromptsCapability {
                    list_changed: false,
                },
            },
            server_info: ServerInfo {
                name: "loom-mcp".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
        }
    }

    /// Request orderly server shutdown: cancels the token that
    /// `mcp_main::serve_until_shutdown` selects on, so the stdio loop
    /// exits and `run` closes the implicit session before returning.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    pub async fn ping(&self) -> serde_json::Value {
        serde_json::json!({})
    }

    pub async fn tools_list(self: &Arc<Self>) -> Vec<crate::tool_cache::Tool> {
        let mut tools = self.tool_cache.list().await;
        if tools.is_empty() {
            // Lazy recovery: an empty cache means every prime so far failed
            // (daemon down at launch and no reconnect yet). Try once more now —
            // `prime` is idempotent and `RpcClient::call` reconnects first if
            // needed — so a daemon that has come up since launch serves a real
            // tool list instead of `[]` until the next reconnect. Failure
            // degrades to the daemon-derived list being empty exactly as
            // before.
            if let Err(e) = self.prime_tool_cache().await {
                self.obs.info(
                    "tools_list_lazy_prime_failed",
                    serde_json::json!({ "code": e.code.as_wire() }),
                );
            } else {
                tools = self.tool_cache.list().await;
            }
        }
        // The session tools are MCP-server-local (schemas defined here, not
        // derived from the daemon's rpc.schemas), so they are advertised
        // even while the daemon-backed cache is cold.
        tools.extend(Self::session_tools());
        tools
    }

    /// The server-local `loom.session.*` tool catalog: schema-driven like
    /// the web.* verbs, but defined here because the daemon's session.*
    /// RPCs are BUILTIN_CORE_METHODS with no registry schemas. These are
    /// the determinism surface for MCP consumers: reset to start a
    /// hermetic, deterministically-anchored session per spec, info to
    /// learn the ids/knobs, diff/validate/export as the regression oracle.
    pub fn session_tools() -> Vec<crate::tool_cache::Tool> {
        use crate::tool_cache::Tool;
        let obj = |properties: serde_json::Value, required: serde_json::Value| {
            serde_json::json!({
                "type": "object",
                "properties": properties,
                "required": required,
                "additionalProperties": false,
            })
        };
        let uint = serde_json::json!({ "type": "integer", "minimum": 0 });
        vec![
            Tool {
                name: TOOL_SESSION_RESET.into(),
                description: "Close the current implicit session and start a fresh one. \
                              Omitted options fall back to the LOOM_MCP_SESSION_* env \
                              baseline, so each reset is hermetic. `budget` sets per-test \
                              limits (e.g. {\"wall_clock\":\"30s\"} or \
                              {\"session_walltime_ms\":30000}) — a runaway page is \
                              budget-killed with error.kind=\"budget_exceeded\". \
                              `no_determinism:true` runs the session on a REAL \
                              wall-clock + unseeded RNG (needed to drive real auth'd \
                              SPAs whose bootstrap timers never fire under the frozen \
                              clock); such a session is NON-replayable. \
                              Returns {session_id}."
                    .into(),
                input_schema: obj(
                    serde_json::json!({
                        "seed": uint,
                        "clock_anchor": uint,
                        "profile": { "type": "string" },
                        "budget": { "type": "object" },
                        "no_determinism": { "type": "boolean" },
                        "audio": { "type": "boolean" },
                    }),
                    serde_json::json!([]),
                ),
            },
            Tool {
                name: TOOL_SESSION_INFO.into(),
                description: "Describe the current implicit session: {session_id, seed, \
                              clock_anchor, profile, budget, created_at_ms, \
                              recreated_count}. recreated_count is the number of times \
                              this MCP process has transparently recreated the implicit \
                              session via eviction self-heal (0 on a fresh process; \
                              excludes loom.session.reset) — pair it with created_at_ms to \
                              detect a recreation. Creates the session lazily if none \
                              exists yet."
                    .into(),
                input_schema: obj(serde_json::json!({}), serde_json::json!([])),
            },
            Tool {
                name: TOOL_SESSION_DIFF.into(),
                description: "Diff the current implicit session (b) against another \
                              session (a = other_session_id, e.g. a previous run). \
                              Returns the DiffReport (field_diffs / screenshot_diffs / \
                              action_count_delta) — the cross-run regression oracle."
                    .into(),
                input_schema: obj(
                    serde_json::json!({
                        "other_session_id": { "type": "string" },
                        "include_screenshots": { "type": "boolean" },
                        "show_dom_diffs": { "type": "boolean" },
                    }),
                    serde_json::json!(["other_session_id"]),
                ),
            },
            Tool {
                name: TOOL_SESSION_VALIDATE.into(),
                description: "Validate a session's manifest hash chain. Defaults to the \
                              current implicit session; returns {session_id, passed, reasons}."
                    .into(),
                input_schema: obj(
                    serde_json::json!({ "session_id": { "type": "string" } }),
                    serde_json::json!([]),
                ),
            },
            Tool {
                name: TOOL_SESSION_EXPORT.into(),
                description: "Export a session's manifest. Defaults to the current \
                              implicit session and format \"json\"; returns {session_id, \
                              format, artifact_ref}."
                    .into(),
                input_schema: obj(
                    serde_json::json!({
                        "session_id": { "type": "string" },
                        "format": { "type": "string" },
                    }),
                    serde_json::json!([]),
                ),
            },
        ]
    }

    /// prime the tool cache from `rpc.schemas`.
    /// Idempotent — repeated calls re-fetch and overwrite. Called at
    /// server startup by `mcp_main::run`, after every reconnect via the
    /// `install_tool_cache_reprime` hook, and lazily by `tools_list`
    /// when the cache is empty; safe to retry on transient daemon-down.
    pub async fn prime_tool_cache(self: &Arc<Self>) -> Result<(), LoomError> {
        self.tool_cache.prime().await
    }

    pub async fn tools_call(
        self: &Arc<Self>,
        p: ToolsCallParams,
    ) -> crate::error_mapper::ToolResult {
        // Server-local session tools are routed before the daemon-derived
        // catalog: they manage the implicit session that the web.* family
        // rides on.
        if let Some(result) = self.try_session_tool(&p).await {
            return result;
        }
        let rpc_method = match crate::tool_cache::ToolCache::mcp_to_rpc_name(&p.name) {
            None => {
                return ErrorMapper::to_tool_result(ErrorMapper::from_unknown_tool(&p.name));
            }
            Some(m) => m.to_string(),
        };
        // Advertised-tool gate (audit 2026-06-10): the prefix strip above is
        // purely a naming transform — membership in the advertised tool list
        // is what authorizes the call. Pre-fix, any "loom."-prefixed name was
        // forwarded verbatim to the daemon, so an MCP client (the model,
        // driven by potentially adversarial page content) could reach
        // credential-mutating and session-destroying verbs (vault.*,
        // session.kill, gc.run, content.get, …) that tools/list never
        // advertised. The server-local loom.session.* tools were already
        // routed above; everything else must be in the primed cache.
        if self.tool_cache.get(&p.name).await.is_none() {
            if self.tool_cache.list().await.is_empty() {
                // Cold cache (daemon down at launch): one lazy prime,
                // mirroring tools_list. A still-unreachable daemon surfaces
                // its transport error rather than a misleading unknown_tool.
                if let Err(e) = self.prime_tool_cache().await {
                    return ErrorMapper::to_tool_result(e);
                }
            }
            if self.tool_cache.get(&p.name).await.is_none() {
                return ErrorMapper::to_tool_result(ErrorMapper::from_unknown_tool(&p.name));
            }
        }
        // Auto-inject the implicit session for tools that need one.
        // The web.* family + their aliases all carry a `session` field;
        // anything else (rpc.schemas, future session-less endpoints) is
        // forwarded verbatim. Caller-supplied `session` takes
        // precedence so power-users can pin a specific session — pinned
        // sessions are never self-healed (we don't own their lifecycle).
        if rpc_method.starts_with("web.") {
            let args = match p.arguments {
                serde_json::Value::Object(m) => m,
                _ => serde_json::Map::new(),
            };
            if !args.contains_key("session") {
                return self.call_with_implicit_session(&rpc_method, args).await;
            }
            return self
                .rpc
                .call_as_tool_result(&rpc_method, serde_json::Value::Object(args))
                .await;
        }
        self.rpc.call_as_tool_result(&rpc_method, p.arguments).await
    }

    /// Inject the implicit session and dispatch, self-healing ONCE when
    /// the daemon reports the session is gone. The idle reaper
    /// (`LOOM_SESSION_IDLE_TTL_SECS`) evicts the implicit session of any
    /// quiet MCP server process; pre-fix the dead cached id made every
    /// subsequent tool call fail with `session_not_found` forever. On
    /// session_not_found/session_closed/session_aborted: drop the cached
    /// id, recreate with the same options, retry the call exactly once.
    /// A failure of the retried call surfaces as-is.
    async fn call_with_implicit_session(
        self: &Arc<Self>,
        rpc_method: &str,
        mut args: serde_json::Map<String, serde_json::Value>,
    ) -> crate::error_mapper::ToolResult {
        let session_id = match self.ensure_implicit_session().await {
            Ok(id) => id,
            Err(e) => return ErrorMapper::to_tool_result(e),
        };
        args.insert(
            "session".to_string(),
            serde_json::Value::String(session_id.clone()),
        );
        let first = self
            .rpc
            .call(rpc_method, serde_json::Value::Object(args.clone()))
            .await;
        let result = match first {
            Err(e) if Self::implicit_session_gone(e.code) => {
                self.obs.info(
                    "implicit_session_gone_recreating",
                    serde_json::json!({ "code": e.code.as_wire(), "method": rpc_method }),
                );
                self.invalidate_implicit_session(&session_id).await;
                match self.ensure_implicit_session().await {
                    Ok(new_id) => {
                        args.insert("session".to_string(), serde_json::Value::String(new_id));
                        self.rpc
                            .call(rpc_method, serde_json::Value::Object(args))
                            .await
                    }
                    Err(create_err) => Err(create_err),
                }
            }
            other => other,
        };
        self.rpc.render_tool_result(rpc_method, result).await
    }

    /// Route `loom.session.*` tool calls to their handlers. Returns
    /// `None` for every other name so `tools_call` falls through to the
    /// daemon-derived catalog.
    async fn try_session_tool(
        self: &Arc<Self>,
        p: &ToolsCallParams,
    ) -> Option<crate::error_mapper::ToolResult> {
        match p.name.as_str() {
            TOOL_SESSION_RESET => Some(self.session_reset(p.arguments.clone()).await),
            TOOL_SESSION_INFO => Some(self.session_info().await),
            TOOL_SESSION_DIFF => Some(self.session_diff(p.arguments.clone()).await),
            TOOL_SESSION_VALIDATE => Some(self.session_validate(p.arguments.clone()).await),
            TOOL_SESSION_EXPORT => Some(self.session_export(p.arguments.clone()).await),
            _ => None,
        }
    }

    /// Parse tool arguments into a typed params struct. MCP clients may
    /// omit `arguments` entirely (→ `Null`); treat that as `{}`.
    fn parse_tool_args<T: serde::de::DeserializeOwned>(
        arguments: serde_json::Value,
    ) -> Result<T, LoomError> {
        let v = if arguments.is_null() {
            serde_json::json!({})
        } else {
            arguments
        };
        serde_json::from_value(v)
            .map_err(|e| ErrorMapper::from_schema_parse(&format!("invalid arguments: {e}")))
    }

    fn ok_tool_result(v: serde_json::Value) -> crate::error_mapper::ToolResult {
        crate::error_mapper::ToolResult {
            is_error: false,
            content: vec![crate::error_mapper::McpContent::from_json(v)],
        }
    }

    /// `loom.session.reset` — close the current implicit session and
    /// start a fresh one. The new options are the env baseline merged
    /// with the explicit arguments; this is what a test runner calls at
    /// the start of each spec to get a hermetic, deterministically-
    /// anchored session. Holds the state lock across close + create so
    /// concurrent tool calls serialize against the swap.
    pub async fn session_reset(
        self: &Arc<Self>,
        arguments: serde_json::Value,
    ) -> crate::error_mapper::ToolResult {
        let p: SessionResetParams = match Self::parse_tool_args(arguments) {
            Ok(p) => p,
            Err(e) => return ErrorMapper::to_tool_result(e),
        };
        let options = SessionOptions {
            seed: p.seed.or(self.baseline_options.seed),
            clock_anchor: p.clock_anchor.or(self.baseline_options.clock_anchor),
            profile: p.profile.or_else(|| self.baseline_options.profile.clone()),
            budget: p.budget.or_else(|| self.baseline_options.budget.clone()),
            no_determinism: p.no_determinism.or(self.baseline_options.no_determinism),
            audio: p.audio.or(self.baseline_options.audio),
        };
        let mut guard = self.implicit_session.lock().await;
        // Close the outgoing session best-effort: an error just means the
        // daemon already reaped it.
        if let Some(old) = guard.session.take() {
            let _ = self
                .rpc
                .call("session.close", serde_json::json!({ "session_id": old.id }))
                .await;
        }
        guard.options = options;
        match self.create_implicit_session(guard.options.clone()).await {
            Ok(created) => {
                let id = created.id.clone();
                guard.session = Some(created);
                Self::ok_tool_result(serde_json::json!({ "session_id": id }))
            }
            // Session stays None: the next tool call lazily recreates with
            // the freshly-stored options.
            Err(e) => ErrorMapper::to_tool_result(e),
        }
    }

    /// `loom.session.info` — current implicit session id + the
    /// determinism knobs it was created with. Creates the session
    /// lazily so a test runner can learn the id before the first verb.
    pub async fn session_info(self: &Arc<Self>) -> crate::error_mapper::ToolResult {
        let mut guard = self.implicit_session.lock().await;
        let session = match self.ensure_locked(&mut guard).await {
            Ok(s) => s,
            Err(e) => return ErrorMapper::to_tool_result(e),
        };
        Self::ok_tool_result(serde_json::json!({
            "session_id": session.id,
            "seed": session.options.seed,
            "clock_anchor": session.options.clock_anchor,
            "profile": session
                .options
                .profile
                .as_deref()
                .unwrap_or(IMPLICIT_SESSION_PROFILE),
            "budget": session.options.budget,
            "created_at_ms": session.created_at_ms,
            "recreated_count": self
                .recreated_count
                .load(std::sync::atomic::Ordering::Relaxed),
        }))
    }

    /// `loom.session.diff` — proxy to the daemon's `session.diff` with
    /// `a = other_session_id` (the baseline run) and `b =` the current
    /// implicit session, so deltas read as "current minus other".
    pub async fn session_diff(
        self: &Arc<Self>,
        arguments: serde_json::Value,
    ) -> crate::error_mapper::ToolResult {
        let p: SessionDiffParams = match Self::parse_tool_args(arguments) {
            Ok(p) => p,
            Err(e) => return ErrorMapper::to_tool_result(e),
        };
        let current = match self.ensure_implicit_session().await {
            Ok(id) => id,
            Err(e) => return ErrorMapper::to_tool_result(e),
        };
        self.rpc
            .call_as_tool_result(
                "session.diff",
                serde_json::json!({
                    "a": p.other_session_id,
                    "b": current,
                    "include_screenshots": p.include_screenshots,
                    "show_dom_diffs": p.show_dom_diffs,
                }),
            )
            .await
    }

    /// `loom.session.validate` — proxy to `session.validate`; defaults
    /// to the current implicit session.
    pub async fn session_validate(
        self: &Arc<Self>,
        arguments: serde_json::Value,
    ) -> crate::error_mapper::ToolResult {
        let p: SessionValidateParams = match Self::parse_tool_args(arguments) {
            Ok(p) => p,
            Err(e) => return ErrorMapper::to_tool_result(e),
        };
        let sid = match self.session_id_or_current(p.session_id).await {
            Ok(id) => id,
            Err(e) => return ErrorMapper::to_tool_result(e),
        };
        self.rpc
            .call_as_tool_result("session.validate", serde_json::json!({ "session_id": sid }))
            .await
    }

    /// `loom.session.export` — proxy to `session.export`; defaults to
    /// the current implicit session and the daemon-default `json` format.
    pub async fn session_export(
        self: &Arc<Self>,
        arguments: serde_json::Value,
    ) -> crate::error_mapper::ToolResult {
        let p: SessionExportParams = match Self::parse_tool_args(arguments) {
            Ok(p) => p,
            Err(e) => return ErrorMapper::to_tool_result(e),
        };
        let sid = match self.session_id_or_current(p.session_id).await {
            Ok(id) => id,
            Err(e) => return ErrorMapper::to_tool_result(e),
        };
        self.rpc
            .call_as_tool_result(
                "session.export",
                serde_json::json!({
                    "session_id": sid,
                    "format": p.format.unwrap_or_else(|| "json".to_string()),
                }),
            )
            .await
    }

    /// Resolve an optional explicit session id, falling back to the
    /// (lazily created) implicit session.
    async fn session_id_or_current(
        self: &Arc<Self>,
        explicit: Option<String>,
    ) -> Result<String, LoomError> {
        match explicit {
            Some(id) => Ok(id),
            None => self.ensure_implicit_session().await,
        }
    }

    pub async fn resources_list(
        self: &Arc<Self>,
    ) -> Result<Vec<crate::resource_tracker::Resource>, LoomError> {
        self.resource_tracker.list().await
    }

    pub async fn resources_read(
        self: &Arc<Self>,
        p: ResourcesReadParams,
    ) -> Result<crate::resource_tracker::ResourceContents, LoomError> {
        self.resource_tracker.read(&p.uri).await
    }

    pub async fn prompts_list(&self) -> Vec<serde_json::Value> {
        Vec::new()
    }

    pub fn unknown_method_error(method: &str) -> McpProtocolError {
        McpProtocolError {
            code: crate::stdio_transport::ERROR_METHOD_NOT_FOUND,
            message: format!("unknown MCP method: {method}"),
            data: None,
        }
    }
}
