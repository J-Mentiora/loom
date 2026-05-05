//! `rpc_handlers` — see `systems/loom-rpc/modules/rpc_handlers/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod rpc_handlers;
pub use rpc_handlers::*;

#[cfg(test)]
mod interface_tests;

use crate::core_service_adapter::core_service_adapter::{
    ContentData, CoreServiceAdapterApi, CreateSessionParams, DiffReport, ExportInfo, GcRunReport,
    GrantInfo, GrantParams, PlaywrightImportInfo, SessionInfo, SessionInspection, ValidationResult,
    VaultAddInfo, VaultAddParams,
};
use crate::error_translator::error_translator::LoomErrorCode;
use crate::host_service_adapter::host_service_adapter::{Action, HostServiceAdapterApi, Receipt};
use crate::rpc_observability::rpc_observability::RpcObservabilityApi;
use crate::schema_provider::schema_provider::{SchemaProviderApi, SchemaRegistry};
use crate::schema_validator::schema_validator::SchemaValidatorApi;
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

    pub async fn session_create(&self, p: CreateSessionParams) -> HandlerResult<SessionInfo> {
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
        self.core.create_session(p).map_err(|code| JsonRpcError {
            code,
            message: "session.create failed".to_string(),
            data: None,
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

    pub async fn session_replay(
        &self,
        s: String,
        sp: Option<f32>,
        nm: Option<String>,
    ) -> HandlerResult<SessionInfo> {
        self.core
            .replay_session(&s, sp, nm.as_deref())
            .map_err(|code| JsonRpcError {
                code,
                message: format!("session.replay failed for session {s}"),
                data: None,
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

    pub async fn action_dispatch(&self, a: Action) -> HandlerResult<Receipt> {
        self.host
            .dispatch_action(a)
            .await
            .map_err(|code| JsonRpcError {
                code,
                message: "action dispatch failed".to_string(),
                data: None,
            })
    }

    // === Vault methods — wired through CoreServiceAdapter to loom-core ===

    pub async fn vault_grant(&self, p: GrantParams) -> HandlerResult<GrantInfo> {
        self.core.vault_grant(p).map_err(|code| JsonRpcError {
            code,
            message: "vault.grant failed".to_string(),
            data: None,
        })
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
}
