//! `rpc_handlers` — see crate root.
pub mod rpc_handlers;
pub use rpc_handlers::*;

#[cfg(test)]
mod interface_tests;

use crate::core_service_adapter::core_service_adapter::{
    ContentData, CoreServiceAdapterApi, CreateSessionParams, DiffReport, ExportInfo, GcRunReport,
    GrantInfo, GrantParams, PlaywrightImportInfo, ReapReport, SessionInfo, SessionInspection,
    ValidationResult, VaultAddInfo, VaultAddParams, VaultDeleteSecretInfo, VaultDeleteSecretParams,
    VaultDiagnoseInfo, VaultGetSessionContextInfo, VaultListLabelsInfo, VaultListLabelsParams,
    VaultSetSecretInfo, VaultSetSecretParams,
};
use crate::error_translator::error_translator::LoomErrorCode;
use crate::host_service_adapter::host_service_adapter::{Action, HostServiceAdapterApi, Receipt};
use crate::rpc_observability::rpc_observability::RpcObservabilityApi;
use crate::schema_provider::schema_provider::{SchemaProviderApi, SchemaRegistry};
use crate::schema_validator::schema_validator::{SchemaValidatorApi, ValidationOutcome};
use std::sync::Arc;

impl RpcHandlers {
    pub fn new(
        core: Arc<dyn CoreServiceAdapterApi>,
        host: Arc<dyn HostServiceAdapterApi>,
        schemas: Arc<dyn SchemaProviderApi>,
        validator: Arc<dyn SchemaValidatorApi>,
        observability: Arc<dyn RpcObservabilityApi>,
    ) -> Arc<Self> {
        Arc::new(Self {
            core,
            host,
            schemas,
            validator,
            observability,
            session_shutdown: std::sync::OnceLock::new(),
            health_provider: std::sync::OnceLock::new(),
            daemon_health_async: std::sync::OnceLock::new(),
        })
    }

    /// `rpc.schemas` — returns the in-memory schema registry.
    pub async fn rpc_schemas(&self) -> HandlerResult<SchemaRegistry> {
        Ok(self.schemas.get_registry_snapshot())
    }

    /// Serialise a handler result to canonical JSON string (RFC 8785).
    pub fn serialise_canonical<T: serde::Serialize>(val: &T) -> Result<String, JsonRpcError> {
        serde_jcs::to_string(val).map_err(|e| JsonRpcError {
            code: LoomErrorCode::InternalError,
            message: format!("canonical serialisation failed: {}", e),
            data: None,
        })
    }

    // === Session method stubs — implemented by session-* features ===

    pub async fn session_create(&self, mut p: CreateSessionParams) -> HandlerResult<SessionInfo> {
        // SDK zero-config compat: both SDKs send `profile: "default"`
        // when the caller doesn't choose one. Resolve the alias to the
        // canonical server default ("safe" — identical to omitting the
        // field, the CLI's no-`--profile` shape) BEFORE validation so
        // the allowlist and every downstream `profile == "safe"`
        // consumer only ever see canonical names.
        p.profile =
            loom_core::profile_registry::profile_registry::resolve_profile_alias(&p.profile)
                .to_string();
        // Typed business validation runs before delegating to the adapter,
        // so bogus profile / network-mode / budget-key values are rejected
        // with typed envelopes carrying the canonical allowlist in `data`.
        crate::session_validation::session_validation::validate_create_session_params(&p)?;
        // Fail-fast when no chromium binary was resolved at
        // daemon boot. Without this check, session.create succeeds and
        // the failure surfaces only on first action (an opaque
        // `shim-failure: action dispatch failed` from spawn ENOENT). The
        // CLI maps `browser_not_found` to a platform-aware install hint.
        if !self.host.has_chromium() {
            return Err(JsonRpcError {
                code: LoomErrorCode::BrowserNotFound,
                message: "no chromium binary found by the resolver — \
                          install via brew/apt/dnf or run 'loom postinstall'"
                    .to_string(),
                data: None,
            });
        }
        self.core.create_session(p).map_err(|e| {
            // The cap rejection is typed end-to-end: route the full
            // LoomError (message + {active, cap, hint} context) through the
            // single ErrorTranslator conversion point so CLI --json, SDKs
            // and raw RPC all see `session_cap_exceeded` with its data —
            // never the opaque `internal_error: session.create failed`.
            // Other codes keep the legacy generic message (their LoomError
            // text is not wire-vetted; see the import.playwright precedent).
            use crate::error_translator::error_translator::{ErrorTranslator, LoomErrorRef};
            match e.code {
                LoomErrorCode::SessionCapExceeded => {
                    ErrorTranslator::from_loom_error(&LoomErrorRef(&e))
                }
                code => JsonRpcError {
                    code,
                    message: "session.create failed".to_string(),
                    data: None,
                },
            }
        })
    }

    pub async fn session_inspect(
        &self,
        s: String,
        at: Option<u64>,
    ) -> HandlerResult<SessionInspection> {
        self.core
            .inspect_session(&s, at)
            .map_err(|code| JsonRpcError {
                code,
                message: format!("session.inspect failed for session {s}"),
                data: None,
            })
    }

    pub async fn session_list(&self) -> HandlerResult<Vec<SessionInfo>> {
        self.core.list_sessions().map_err(|code| JsonRpcError {
            code,
            message: "session.list failed".to_string(),
            data: None,
        })
    }

    pub async fn session_close(&self, s: String) -> HandlerResult<SessionInfo> {
        self.core.close_session(&s).map_err(|code| JsonRpcError {
            code,
            message: format!("session.close failed for session {s}"),
            data: None,
        })
    }

    pub async fn session_abort(&self, s: String, r: String) -> HandlerResult<SessionInfo> {
        self.core
            .abort_session(&s, &r)
            .map_err(|code| JsonRpcError {
                code,
                message: format!("session.abort failed for session {s}"),
                data: None,
            })
    }

    /// `daemon.health` — operational snapshot. Shallow path is non-
    /// blocking: returns active session count, per-shim breaker state,
    /// OTel exporter status. Deep path (`{deep: true}`) probes each
    /// running shim for self-reported uptime + requests-served counters
    /// (1 s per-shim budget, fanned out concurrently); the daemon
    /// supplements with its own restart bookkeeping.
    ///
    /// Overall budget on the deep path is `LOOM_DEEP_HEALTH_BUDGET_MS`
    /// (default 3000) — a safety net beyond the per-shim 1 s. On
    /// timeout, returns whatever results were ready and marks the rest
    /// implicitly via the shallow `deep: None` fallback in the next
    /// poll (callers can re-issue).
    ///
    /// When no provider is wired (test stubs), returns an empty snapshot
    /// with `otel_exporter: "unwired"`.
    pub async fn daemon_health(
        &self,
        deep: bool,
    ) -> HandlerResult<crate::rpc_handlers::rpc_handlers::DaemonHealth> {
        use crate::rpc_handlers::rpc_handlers::DaemonHealth;
        // Shallow path: synchronous provider snapshot.
        let mut health = self
            .health_provider
            .get()
            .map(|p| p.snapshot(deep))
            .unwrap_or_else(|| DaemonHealth {
                active_sessions: 0,
                shim_breaker_states: Vec::new(),
                otel_exporter: "unwired".to_string(),
                orphan_browser_trees: 0,
                oldest_active_session_age_secs: None,
                deep: None,
            });

        // Deep path: only if the caller asked AND an async provider is
        // wired. The sync `snapshot(deep)` call above does NOT populate
        // `deep` — that's by design; the deep path is async and lives
        // here in the handler.
        if deep {
            if let Some(async_provider) = self.daemon_health_async.get() {
                let budget_ms = std::env::var("LOOM_DEEP_HEALTH_BUDGET_MS")
                    .ok()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(3_000);
                let deep_results = tokio::time::timeout(
                    std::time::Duration::from_millis(budget_ms),
                    async_provider.snapshot_deep(),
                )
                .await
                .unwrap_or_default();
                health.deep = Some(deep_results);
            }
        }
        Ok(health)
    }

    /// `session.kill` — admin escape hatch for stuck sessions.
    /// Performs the existing abort flow (set abort flag → status Aborted
    /// → SessionTerminal manifest entry) AND blocks until the shim
    /// child is reaped, with a 5 s ceiling, then escalates to SIGKILL
    /// (handled inside `WasmHost::shutdown_session` → `shutdown_process`
    /// per D12). The harness use case: "I'm done with this session, the
    /// shim wedged, I want to know cleanup is done before reusing the
    /// session_id." Pure abort would leave that as a fire-and-forget
    /// hope; kill makes it a hard guarantee.
    pub async fn session_kill(&self, s: String) -> HandlerResult<SessionInfo> {
        let started = std::time::Instant::now();
        tracing::info!(
            metric = "loom_daemon_session_kill_start",
            session_id = %s,
            "session.kill received"
        );

        // Step 1: synchronous abort. Sets abort flag, transitions status,
        // appends SessionTerminal, calls scope.cancel() (which signals
        // budget timer + receipt spawns + ShimProcess inner tasks).
        let info = self
            .core
            .abort_session(&s, "killed")
            .map_err(|code| JsonRpcError {
                code,
                message: format!("session.kill failed during abort phase for session {s}"),
                data: None,
            })?;

        // Step 2: synchronous shim teardown via the optional async
        // driver wired by the daemon. If unset (test stubs), kill
        // degrades to abort-only — the caller still gets a typed
        // SessionInfo back, just without the synchronous-teardown
        // guarantee.
        if let Some(shutdown) = self.session_shutdown.get() {
            shutdown.shutdown_with_ceiling(&s, 5_000).await;
        }

        tracing::info!(
            metric = "loom_daemon_session_kill_done",
            session_id = %s,
            elapsed_ms = started.elapsed().as_millis() as u64,
            shutdown_wired = self.session_shutdown.get().is_some(),
        );
        Ok(info)
    }

    pub async fn session_replay(
        &self,
        s: String,
        sp: Option<f32>,
        nm: Option<String>,
    ) -> HandlerResult<SessionInfo> {
        self.core
            .replay_session(&s, sp, nm.as_deref())
            .map_err(|e| {
                // Replay-refusal fidelity: the adapter hands back a full
                // `LoomError`, so surface its typed code AND its compiled-in
                // human refusal reason (e.g. the `--no-determinism` / crashed-
                // source / missing-blob explanations from loom-core's replay
                // engine) instead of the old generic template. The generic
                // message remains only as a fallback for empty messages.
                let message = if e.message.is_empty() {
                    format!("session.replay failed for session {s}")
                } else {
                    truncate_wire_message(e.message)
                };
                JsonRpcError {
                    code: e.code,
                    message,
                    data: e.context,
                }
            })
    }

    pub async fn session_diff(
        &self,
        a: String,
        b: String,
        i: bool,
        d: bool,
    ) -> HandlerResult<DiffReport> {
        self.core
            .diff_sessions(&a, &b, i, d)
            .map_err(|code| JsonRpcError {
                code,
                message: format!("session.diff failed for sessions {a} vs {b}"),
                data: None,
            })
    }

    pub async fn session_export(&self, s: String, f: String) -> HandlerResult<ExportInfo> {
        self.core.export_session(&s, &f).map_err(|code| {
            // For SchemaViolation (mapped from
            // LoomErrorCode::InvalidArgument when the daemon
            // rejects an unsupported format), surface a more
            // actionable message that names the format the user
            // tried to export to. Other codes use the generic
            // "session.export failed" template.
            let message = match code {
                crate::error_translator::error_translator::LoomErrorCode::SchemaViolation => {
                    format!("session.export rejected format '{f}' (supported: json, tarball, har)")
                }
                _ => format!("session.export failed for session {s}"),
            };
            JsonRpcError {
                code,
                message,
                data: None,
            }
        })
    }

    pub async fn content_get(&self, artifact_ref: String) -> HandlerResult<ContentData> {
        // Reject implausible refs before they reach the content store:
        // content.get is a builtin core method (no JSON-schema validation),
        // so artifact_ref arrives here completely unconstrained. Same
        // SchemaViolation shape as the import.playwright bad-hex arm.
        if !is_plausible_sha256(&artifact_ref) {
            return Err(JsonRpcError {
                code: LoomErrorCode::SchemaViolation,
                message: format!(
                    "content.get: artifact_ref must be a 64-char lowercase hex sha256 (got {} bytes)",
                    artifact_ref.len()
                ),
                data: None,
            });
        }
        self.core
            .content_get(&artifact_ref)
            .map_err(|code| JsonRpcError {
                code,
                message: format!("content.get failed for ref {artifact_ref}"),
                data: None,
            })
    }

    pub async fn session_validate(&self, s: String) -> HandlerResult<ValidationResult> {
        self.core.validate_session(&s).map_err(|code| JsonRpcError {
            code,
            message: format!("session.validate failed for session {s}"),
            data: None,
        })
    }

    /// `import.playwright` — decode hex-encoded trace bytes and forward to
    /// the core importer.
    pub async fn import_playwright(
        &self,
        trace_hex: String,
    ) -> HandlerResult<PlaywrightImportInfo> {
        let bytes = hex::decode(&trace_hex).map_err(|e| JsonRpcError {
            code: LoomErrorCode::SchemaViolation,
            message: format!("import.playwright: trace_hex is not valid hex: {e}"),
            data: None,
        })?;
        self.core.import_playwright(&bytes).map_err(|code| {
            // The core importer raises InvalidArgument (→ SchemaViolation
            // on the wire) for malformed zips and missing `trace.trace`
            // entries — the typed message text is lost at the AdapterError
            // boundary today, so name the most likely cause here so the
            // user knows where to look.
            let message = match code {
                LoomErrorCode::SchemaViolation => {
                    "import.playwright: trace.zip is invalid or missing a trace.trace entry"
                        .to_string()
                }
                _ => "import.playwright failed".to_string(),
            };
            JsonRpcError {
                code,
                message,
                data: None,
            }
        })
    }

    // === Action method stub — implemented by wasm-host features ===

    pub async fn action_dispatch(
        &self,
        a: Action,
        deadline_ms: Option<u64>,
    ) -> HandlerResult<Receipt> {
        self.host
            .dispatch_action(a, deadline_ms)
            .await
            .map_err(|code| JsonRpcError {
                code,
                message: "action dispatch failed".to_string(),
                data: None,
            })
    }

    // === Vault methods — wired through CoreServiceAdapter to loom-core ===

    pub async fn vault_grant(&self, p: GrantParams) -> HandlerResult<GrantInfo> {
        let info = self.core.vault_grant(p).map_err(|code| JsonRpcError {
            code,
            message: "vault.grant failed".to_string(),
            data: None,
        })?;
        // Belt+braces response check documented in this module's
        // contract and on `GrantInfo`: validate the response against
        // the registered `vault.grant` response schema so secret-shaped
        // fields can never leak onto the wire. No registered response
        // schema = Pass (validator contract), so installs without a
        // vault.grant response schema behave exactly as before.
        let response = serde_json::to_value(&info).map_err(|e| JsonRpcError {
            code: LoomErrorCode::InternalError,
            message: format!("vault.grant response serialisation failed: {e}"),
            data: None,
        })?;
        match self.validator.validate_response("vault.grant", &response) {
            ValidationOutcome::Pass => Ok(info),
            ValidationOutcome::Violation(err) | ValidationOutcome::MethodNotFound(err) => {
                tracing::error!(
                    reason = %err.message,
                    "vault.grant response failed response-schema validation — \
                     refusing to return it"
                );
                Err(JsonRpcError {
                    code: LoomErrorCode::InternalError,
                    message: "vault.grant response failed response-schema validation".to_string(),
                    data: None,
                })
            }
        }
    }

    pub async fn vault_revoke(&self, g: String, r: String) -> HandlerResult<()> {
        self.core.vault_revoke(&g, &r).map_err(|code| JsonRpcError {
            code,
            message: format!("vault.revoke failed for grant {g}"),
            data: None,
        })
    }

    pub async fn vault_list_grants(&self, s: Option<String>) -> HandlerResult<Vec<GrantInfo>> {
        self.core
            .vault_list_grants(s.as_deref())
            .map_err(|code| JsonRpcError {
                code,
                message: "vault.list_grants failed".to_string(),
                data: None,
            })
    }

    pub async fn vault_add(&self, p: VaultAddParams) -> HandlerResult<VaultAddInfo> {
        self.core.vault_add(p).map_err(|code| JsonRpcError {
            code,
            message: "vault.add failed".to_string(),
            data: None,
        })
    }

    pub async fn vault_set_secret(
        &self,
        p: VaultSetSecretParams,
    ) -> HandlerResult<VaultSetSecretInfo> {
        let label = p.label.clone();
        self.core.vault_set_secret(p).map_err(|code| JsonRpcError {
            code,
            message: format!("vault.set_secret failed for label '{label}'"),
            data: None,
        })
    }

    pub async fn vault_delete_secret(
        &self,
        p: VaultDeleteSecretParams,
    ) -> HandlerResult<VaultDeleteSecretInfo> {
        let label = p.label.clone();
        self.core
            .vault_delete_secret(p)
            .map_err(|code| JsonRpcError {
                code,
                message: format!("vault.delete_secret failed for label '{label}'"),
                data: None,
            })
    }

    pub async fn vault_list_labels(
        &self,
        p: VaultListLabelsParams,
    ) -> HandlerResult<VaultListLabelsInfo> {
        self.core.vault_list_labels(p).map_err(|code| JsonRpcError {
            code,
            message: "vault.list_labels failed".to_string(),
            data: None,
        })
    }

    pub async fn vault_diagnose(&self) -> HandlerResult<VaultDiagnoseInfo> {
        self.core.vault_diagnose().map_err(|code| JsonRpcError {
            code,
            message: "vault.diagnose failed".to_string(),
            data: None,
        })
    }

    pub async fn vault_get_session_context(&self) -> HandlerResult<VaultGetSessionContextInfo> {
        self.core
            .vault_get_session_context()
            .map_err(|code| JsonRpcError {
                code,
                message: "vault.get_session_context failed".to_string(),
                data: None,
            })
    }

    /// `gc.run` — runs GC on the content store. `ttl_days` defaults
    /// to the daemon-configured TTL when None.
    pub async fn gc_run(
        &self,
        ttl_days: Option<u64>,
        store_max_bytes: Option<u64>,
    ) -> HandlerResult<GcRunReport> {
        self.core
            .gc_run(ttl_days, store_max_bytes)
            .map_err(|code| JsonRpcError {
                code,
                message: "gc.run failed".to_string(),
                data: None,
            })
    }

    pub async fn session_reap(&self, dry_run: bool) -> HandlerResult<ReapReport> {
        self.core
            .session_reap(dry_run)
            .map_err(|code| JsonRpcError {
                code,
                message: "session.reap failed".to_string(),
                data: None,
            })
    }
}

/// `true` when `r` is a plausible CAS reference: exactly 64 lowercase hex
/// chars, the content store's addressing scheme (`sha256_hex`).
fn is_plausible_sha256(r: &str) -> bool {
    r.len() == 64 && r.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Enforce the translator's wire contract (`message` ≤ 280 chars,
/// truncated with an ellipsis) on a pass-through message. Char-boundary
/// safe; messages within the limit pass unchanged.
fn truncate_wire_message(msg: String) -> String {
    use crate::error_translator::error_translator::MAX_MESSAGE_LEN;
    if msg.chars().count() <= MAX_MESSAGE_LEN {
        return msg;
    }
    let mut truncated: String = msg.chars().take(MAX_MESSAGE_LEN - 1).collect();
    truncated.push('…');
    truncated
}
