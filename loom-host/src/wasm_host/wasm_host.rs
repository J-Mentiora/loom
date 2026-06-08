// WasmHost — public API facade implementing `loom-host_contract.md`.
//
// # Contract semantics
// - **API-exposed.** Only `WasmHost` is `pub` outside the crate. All
//   other modules are `pub(crate)`.
// - **Loads `.cwasm` artifacts at construction.** `WasmHost::new`
//   populates `ModuleLibrary` from
//   `~/Library/Application Support/loom/surfaces/*.cwasm`. Recovery on
//   load failure is signalled via `loom-core::StartupManager::on_aot_failure`.
// - **Dispatch order.** `WasmHost::dispatch` runs:
//     1. (caller `loom-core::SessionManager` already invoked `BudgetEnforcer::check`)
//     2. `ModuleLibrary::get(action.surface)` — cache lookup, NEVER lazy compile.
//     3. `SessionExecutor::run(...)` — surface invocation on caller's tokio handle.
//     4. Synchronous return of `ActionOutcome` to caller.
//     5. AFTER return: `ReceiptMarshaller::queue(outcome, session.receipt_pool)`
//        — runs on background, calls `BudgetEnforcer::account` then
//        `ManifestWriter::append`.
// - **Off-hot-path receipt.** Step 5 happens AFTER the
//   sync return — no `fsync`, no manifest append on the dispatch path.
// - **Mode plumbing.** `WasmHost::new(core, mode)`
//   captures the default mode for sessions that don't override.
//   `dispatch` selects `HostFunctionRegistry::linker_for(mode)` based
//   on `SessionHandle.mode_override`.
// - **`compile_module`.** Entry for
//   `loom-cli postinstall` and `StartupManager::recover_surface`. NOT
//   reachable from `dispatch`.

use crate::compiler::Compiler;
use crate::host_function_registry::HostFunctionRegistry;
use crate::host_observability::HostObservability;
use crate::module_library::ModuleLibrary;
use crate::receipt_marshaller::ReceiptMarshaller;
use crate::session_executor::{Action, ActionOutcome, SessionExecutor, SessionHandle};
use crate::shim_manager::ShimManager;
use crate::trap_handler::TrapHandler;
use crate::wasm_runtime::{WasmRuntime, WasmRuntimeConfig};
use crate::wit_type_marshaller::Mode;
use loom_core::core_api_facade::CoreApiFacade;
use loom_core::error::LoomError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Per-process configuration. Threaded in by the binary entry point
/// (config-precedence: CLI > env > config > defaults).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostConfig {
    pub surfaces_dir: PathBuf,
    pub runtime: WasmRuntimeConfig,
    pub redaction_enabled: bool,
    pub default_mode: Mode,
    /// Optional chromium shim template config. Set by the daemon at
    /// boot once the verified Chromium binary path + shim binary path
    /// are resolved. None disables the chromium surface.
    #[serde(default)]
    pub shim_chromium: Option<ShimChromiumConfig>,
}

/// Per-shim configuration for the chromium shim. Registered as a
/// template under bare ShimId("chromium"); the host derives per-session
/// ShimIds via `format!("chromium:{session_id}")`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShimChromiumConfig {
    /// Path to the `loom-shim-chromium` binary (alongside the daemon).
    pub shim_binary_path: PathBuf,
    /// Path to the verified Chromium binary (resolved by `loom postinstall`,
    /// SHA-pinned). Passed to the shim via `LOOM_SHIM_CHROMIUM_PATH` env.
    pub chromium_path: PathBuf,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            surfaces_dir: PathBuf::from("~/Library/Application Support/loom/surfaces"),
            runtime: WasmRuntimeConfig::default(),
            redaction_enabled: true,
            default_mode: Mode::Live,
            shim_chromium: None,
        }
    }
}

/// The public facade. Held by the binary entry point as
/// `Arc<WasmHost>`.
pub struct WasmHost {
    pub(crate) core: Arc<CoreApiFacade>,
    pub(crate) runtime: Arc<WasmRuntime>,
    pub(crate) library: Arc<ModuleLibrary>,
    pub(crate) registry: Arc<HostFunctionRegistry>,
    pub(crate) compiler: Compiler,
    pub(crate) executor: Arc<SessionExecutor>,
    pub(crate) trap_handler: Arc<TrapHandler>,
    pub(crate) receipts: Arc<ReceiptMarshaller>,
    pub(crate) shim: Arc<ShimManager>,
    pub(crate) obs: Arc<HostObservability>,
    pub(crate) default_mode: Mode,
}

impl WasmHost {
    /// Construct + load. Pre/Post per `loom-host_contract.md`.
    /// Loads pre-compiled (AOT) WASM modules from
    /// `~/Library/Application Support/loom/surfaces/<name>.cwasm`.
    /// Cache-miss / corrupt-artifact recovery is signalled via
    /// `core.startup_manager().on_aot_failure(...)`.
    pub fn new(core: Arc<CoreApiFacade>, config: HostConfig) -> Result<Arc<Self>, LoomError> {
        let runtime = WasmRuntime::new(config.runtime.clone())?;
        let library = ModuleLibrary::new(runtime.clone(), config.surfaces_dir.clone());
        let load_failures = library.load_all()?;
        // Per-surface load failures are NOT propagated as Err (design §3.3
        // — one bad artifact doesn't block the rest). But they must not be
        // silent: every failure gets a tracing::warn! so operators see the
        // surface-not-loaded story BEFORE the first action dispatch trips
        // on it. (Was a debug-via-CI-artifact-only signal before; raised
        // post-#93 ship review.) Also log the surfaces_dir + the resolved
        // file list so the "is postinstall writing to the right place"
        // question can be answered from the log alone.
        let cwasm_paths: Vec<String> = std::fs::read_dir(&config.surfaces_dir)
            .map(|d| {
                d.flatten()
                    .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("cwasm"))
                    .map(|e| e.path().display().to_string())
                    .collect()
            })
            .unwrap_or_default();
        tracing::info!(
            surfaces_dir = %config.surfaces_dir.display(),
            cwasm_files = ?cwasm_paths,
            loaded_count = library.loaded_count(),
            failure_count = load_failures.len(),
            "WasmHost::new: ModuleLibrary::load_all completed"
        );
        for f in &load_failures {
            tracing::warn!(
                surface = %f.surface.0,
                artifact_path = %f.artifact_path.display(),
                error_code = %f.error_code,
                details = %f.details,
                "WasmHost::new: surface failed to load (will trap on first dispatch)"
            );
        }
        let obs = HostObservability::new(config.redaction_enabled);
        let receipts = ReceiptMarshaller::new(core.manifest_writer(), core.budget_enforcer());
        let trap_handler = TrapHandler::new(obs.clone(), receipts.clone());
        let executor = SessionExecutor::new(
            runtime.clone(),
            library.clone(),
            trap_handler.clone(),
            obs.clone(),
        );
        let registry = HostFunctionRegistry::new(runtime.engine())?;
        let compiler = crate::compiler::Compiler::new(runtime.clone());
        let shim = ShimManager::new(obs.clone());
        // Register the chromium template under bare ShimId("chromium").
        // host_function_table::shim_call clones it per session and
        // appends LOOM_SHIM_USER_DATA_DIR (L11).
        if let Some(shim_chromium) = &config.shim_chromium {
            let mut env = vec![(
                "LOOM_SHIM_CHROMIUM_PATH".to_string(),
                shim_chromium.chromium_path.display().to_string(),
            )];
            // The host doesn't know the session_id at template time;
            // L11's lazy register appends LOOM_SHIM_USER_DATA_DIR on
            // first use.
            let template = crate::shim_manager::ShimConfig {
                binary_path: shim_chromium.shim_binary_path.clone(),
                args: vec![],
                env: std::mem::take(&mut env),
                spawn_retry: 1,
                breaker_threshold: 3,
                breaker_open_ms: 5_000,
                send_timeout_ms: 5_000,
                recv_timeout_ms: 30_000,
            };
            shim.register(crate::shim_manager::ShimId("chromium".into()), template);
        }
        Ok(Arc::new(Self {
            core,
            runtime,
            library,
            registry,
            compiler,
            executor,
            trap_handler,
            receipts,
            shim,
            obs,
            default_mode: config.default_mode,
        }))
    }

    /// Dispatch an action to the appropriate surface module.
    ///
    /// **Pre-conditions** (caller's responsibility):
    ///   - `BudgetEnforcer::check(session_id, action)` already passed.
    ///   - `core.session_manager().get(session_id)` is `SessionStatus::Active`.
    ///
    /// **Post-conditions:**
    ///   - On `Ok(ActionOutcome)`: receipt already queued via
    ///     `ReceiptMarshaller::queue` on `session.receipt_pool`.
    ///   - On `Err(LoomError)`: receipt was emitted (trap path) or
    ///     surface unavailable.
    ///
    /// Tear down all shim subprocesses bound to `session_id`. Called by
    /// the daemon's session-close handler. Sends a Shutdown frame to
    /// each child + waits with SIGTERM/SIGKILL fallback. Idempotent —
    /// safe to call after the session has already been closed.
    pub async fn shutdown_session(&self, session_id: &str) {
        self.shim.shutdown_session(session_id).await;
    }

    /// **SLA:** receipt-overhead p95 ≤ 50 ms above underlying CDP/shim
    /// call.
    pub async fn dispatch(
        self: &Arc<Self>,
        action: Action,
        session: SessionHandle,
    ) -> Result<ActionOutcome, LoomError> {
        use crate::receipt_marshaller::ActionOutcome as ReceiptOutcome;
        use crate::receipt_marshaller::ObservedCosts;

        // 1. Check surface is loaded (never compile on dispatch path)
        let _ = self
            .library
            .get(&crate::module_library::SurfaceName(action.surface.clone()))?;

        // 2. Select mode and linker
        let mode = self.default_mode;
        let linker = self.registry.linker_for(mode);

        // 3. Build per-action HostState
        let determinism = self.core.determinism();
        let tape_writer = determinism.new_tape_writer();
        let host_state = crate::host_function_table::HostState {
            core: self.core.clone(),
            determinism,
            budget: self.core.budget_enforcer(),
            session_id: session.session_id.clone(),
            action_id: action.action_id,
            mode,
            receipt_builder: crate::receipt_marshaller::ReceiptBuilder {
                action_id: action.action_id,
                ..Default::default()
            },
            side_effects: crate::receipt_marshaller::SideEffectAccumulator::default(),
            host_call_metrics: Vec::new(),
            shim_manager: self.shim.clone(),
            obs: self.obs.clone(),
            tape_writer,
            replay_table: None,
            wasi_ctx: crate::host_function_table::build_sandboxed_wasi_ctx(),
            wasi_table: wasmtime::component::ResourceTable::new(),
            seed: session.seed,
            epoch_ms: session.epoch_ms,
            no_blocklist: session.no_blocklist,
            profile: session.profile.clone(),
            downloads_dir: session.downloads_dir.clone(),
        };

        // 4. Run surface invocation
        let receipt_pool = session.receipt_pool.clone();
        let session_id = session.session_id.clone();
        let outcome = self
            .executor
            .run(action, session, mode, linker, host_state)
            .await?;

        // 5. Queue receipt off dispatch path
        let (builder, costs) = match &outcome {
            ActionOutcome::Success {
                builder,
                observed_costs,
            } => (builder.clone(), observed_costs.clone()),
            ActionOutcome::Trapped { builder, .. } | ActionOutcome::Aborted { builder } => {
                (builder.clone(), ObservedCosts::default())
            }
        };
        let _ = self.receipts.queue(
            ReceiptOutcome {
                session_id,
                builder,
                observed_costs: costs,
            },
            receipt_pool,
        );

        Ok(outcome)
    }

    /// AOT-compile a WASM module on install or postinstall.
    /// Caches at `~/Library/Application Support/loom/surfaces/<name>.cwasm`.
    /// SLA: ~250 ms per module (Cranelift cold cache); not on hot path.
    pub fn compile_module(&self, source: &Path, dest: &Path) -> Result<(), LoomError> {
        self.compiler.compile_module(source, dest)
    }

    /// Read the configured default mode. Used by binary entry point for
    /// diagnostics.
    pub fn default_mode(&self) -> Mode {
        self.default_mode
    }

    /// Accessor: the loaded library. Used by `loom-cli loom diag`.
    pub fn library(&self) -> Arc<ModuleLibrary> {
        self.library.clone()
    }

    /// Accessor: the live `ShimManager` snapshot. Used by `daemon.health`
    /// to read per-shim circuit-breaker state without crossing the
    /// `wasm_host_handle` boundary every call.
    pub fn shim_manager(&self) -> Arc<ShimManager> {
        self.shim.clone()
    }

    /// Read the session's full-capture network entries observed since the last
    /// navigate (the `loom.web.network_log` tool). Observation-only — does NOT
    /// run the WASM guest, navigate, or touch the replay hash chain. When the
    /// serialized list ≥ 64KB it is offloaded to the content store and returned
    /// as a `network_entries_blob_ref`. A missing shim (no navigate yet) yields
    /// an empty result rather than spawning Chromium.
    pub async fn network_log(&self, session_id: &str) -> Result<NetworkLogData, LoomError> {
        use crate::shim_manager::ShimId;
        let effective_id = ShimId(format!("chromium:{session_id}"));
        if !self.shim.is_registered(&effective_id) {
            return Ok(NetworkLogData::default());
        }
        let shim_session_id = self.shim.shim_session_id_for(session_id);
        let outcome = self
            .shim
            .send_network_log(effective_id, shim_session_id, 0)
            .await?;

        const NETWORK_ENTRIES_INLINE_THRESHOLD: usize = 65_536;
        let bytes = serde_json::to_vec(&outcome.network_entries).unwrap_or_default();
        if bytes.len() >= NETWORK_ENTRIES_INLINE_THRESHOLD {
            match self.core.content_store.put(&bytes) {
                Ok(cref) => Ok(NetworkLogData {
                    network_entries: Vec::new(),
                    network_entries_blob_ref: Some(cref.sha256),
                    network_entries_truncated: outcome.network_entries_truncated,
                }),
                Err(e) => {
                    // Graceful degrade: drop the list (count + session only, no URLs).
                    tracing::warn!(
                        session_id = %session_id,
                        entry_count = outcome.network_entries.len(),
                        error = %e,
                        "network_log content-store offload failed; dropping list"
                    );
                    Ok(NetworkLogData {
                        network_entries: Vec::new(),
                        network_entries_blob_ref: None,
                        network_entries_truncated: true,
                    })
                }
            }
        } else {
            let entries = outcome
                .network_entries
                .iter()
                .filter_map(|e| serde_json::to_value(e).ok())
                .collect();
            Ok(NetworkLogData {
                network_entries: entries,
                network_entries_blob_ref: None,
                network_entries_truncated: outcome.network_entries_truncated,
            })
        }
    }
}

/// Result of [`WasmHost::network_log`]. Mirrors the wire `Receipt`'s
/// network-entries fields: exactly one of `network_entries` (inline) /
/// `network_entries_blob_ref` (offloaded) carries data.
#[derive(Debug, Default, Clone)]
pub struct NetworkLogData {
    pub network_entries: Vec<serde_json::Value>,
    pub network_entries_blob_ref: Option<String>,
    pub network_entries_truncated: bool,
}
