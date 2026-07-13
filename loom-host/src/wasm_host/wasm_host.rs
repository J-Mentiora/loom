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
use crate::network_offload::{offload_or_inline_network_entries, NetworkEntriesPayload};
use crate::receipt_marshaller::ReceiptMarshaller;
use crate::session_executor::{Action, ActionOutcome, SessionExecutor, SessionHandle};
use crate::shim_manager::ShimManager;
use crate::trap_handler::TrapHandler;
use crate::wasm_runtime::{WasmRuntime, WasmRuntimeConfig};
use crate::wit_type_marshaller::Mode;
use loom_core::core_api_facade::CoreApiFacade;
use loom_core::error::{LoomError, LoomErrorCode};
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
        let trap_handler = TrapHandler::new(obs.clone());
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
    ///   - On `Ok(ActionOutcome)`: the action's SINGLE receipt is queued
    ///     via `ReceiptMarshaller::queue` on `session.receipt_pool` —
    ///     for `Trapped`/`Aborted` outcomes the builder carries the
    ///     truthful non-Ok status (no separate trap-handler append).
    ///   - On `Err(LoomError)`: surface unavailable / non-trap dispatch
    ///     failure; no receipt is queued.
    ///
    /// Tear down all shim subprocesses bound to `session_id`. Called by
    /// the daemon's session-close handler. Sends a Shutdown frame to
    /// each child + waits with SIGTERM/SIGKILL fallback. Idempotent —
    /// safe to call after the session has already been closed.
    pub async fn shutdown_session(&self, session_id: &str) {
        // video-capture: finalize any still-active recording (a whole-session
        // recording, or a bracketed one the agent left open) BEFORE tearing down
        // the shim — the encode needs the live subprocess. Best-effort: any
        // failure is logged and never blocks shutdown. When a `.webm` is
        // produced, its hash + metadata are appended to the session's
        // `recordings.jsonl` sidecar so it's retrievable after close.
        if let Ok(result) = self.stop_recording(session_id).await {
            if let Some(hash) = &result.screencast_after_hash {
                self.write_recording_sidecar(session_id, hash, &result);
            }
        }
        self.shim.shutdown_session(session_id).await;
    }

    /// video-capture: append a finalized recording's hash + metadata to the
    /// per-session `recordings.jsonl` sidecar (under the sessions root, NOT in
    /// the manifest WAL — the non-deterministic `.webm` hash must never enter the
    /// hash chain). Best-effort; a write failure is logged, never propagated.
    fn write_recording_sidecar(&self, session_id: &str, hash: &str, result: &ScreencastResult) {
        use std::io::Write as _;
        let dir = self.core.sessions_root.join(session_id);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!(session_id = %session_id, error = %e, "recordings sidecar: mkdir failed");
            return;
        }
        let path = dir.join("recordings.jsonl");
        let line = serde_json::json!({
            "screencast_after_hash": hash,
            "frame_count": result.frame_count,
            "duration_ms": result.duration_ms,
            "stop_reason": result.stop_reason,
        });
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(mut f) => {
                let _ = writeln!(f, "{line}");
            }
            Err(e) => {
                tracing::warn!(session_id = %session_id, error = %e, "recordings sidecar: open failed")
            }
        }
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

        // 3. Build per-action HostState. The determinism harness is the
        // SESSION's own (seeded from Session.seed) — never the facade
        // singleton — so rng_next_u64/clock_now draws are isolated per
        // session and reproducible from the Header-recorded seed.
        let determinism = session.determinism.clone();
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
            no_determinism: session.no_determinism,
            audio: session.audio,
            profile: session.profile.clone(),
            downloads_dir: session.downloads_dir.clone(),
            capture_dom_after: session.capture_dom_after,
            cookie_capture: std::sync::Arc::new(parking_lot::Mutex::new(None)),
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

        // Observational side-channel — shared ≥64KB offload-or-inline + fail-open
        // degrade logic (see `crate::network_offload`, also used by `navigate_execute`).
        let (payload, network_entries_truncated) = offload_or_inline_network_entries(
            &*self.core.content_store,
            &outcome.network_entries,
            outcome.network_entries_truncated,
            session_id,
        );
        let (network_entries, network_entries_blob_ref) = match payload {
            NetworkEntriesPayload::Inline(bytes) => (
                serde_json::from_slice::<Vec<serde_json::Value>>(&bytes).unwrap_or_default(),
                None,
            ),
            NetworkEntriesPayload::Offloaded(cref) => (Vec::new(), Some(cref.sha256)),
            NetworkEntriesPayload::Dropped => (Vec::new(), None),
        };
        Ok(NetworkLogData {
            network_entries,
            network_entries_blob_ref,
            network_entries_truncated,
        })
    }

    /// video-capture: start a `web.start_recording` screencast on the session's
    /// active page target. Errors if no page exists yet (navigate first). Does
    /// NOT spawn Chromium — a recording only makes sense once a page is live.
    pub async fn start_recording(
        &self,
        session_id: &str,
        max_duration_ms: u64,
        max_bytes: u64,
        frame_rate: u32,
    ) -> Result<(), LoomError> {
        use crate::shim_manager::ShimId;
        let effective_id = ShimId(format!("chromium:{session_id}"));
        if !self.shim.is_registered(&effective_id) {
            return Err(LoomError::new(
                LoomErrorCode::SurfaceTrap,
                "no active page target — navigate before web.start_recording",
            ));
        }
        let shim_session_id = self.shim.shim_session_id_for(session_id);
        self.shim
            .send_start_recording(
                effective_id,
                shim_session_id,
                0,
                max_duration_ms,
                max_bytes,
                frame_rate,
            )
            .await
    }

    /// voice-call-io (task 04): inject caller-provided audio bytes into the
    /// session's synthetic microphone. Payload is already resolved + size-bounded
    /// by the daemon; the shim receives raw bytes and hands them to the in-page
    /// enqueue hook. Errors if no page target exists (navigate + join the call
    /// first). Returns the observational [`AudioInjectOutcome`] (duration + whether
    /// playout was awaited); the daemon logs it and builds a constant-hash receipt.
    pub async fn inject_audio(
        &self,
        session_id: &str,
        audio_bytes: Vec<u8>,
        await_playout: bool,
    ) -> Result<loom_shared::navigate_outcome::AudioInjectOutcome, LoomError> {
        use crate::shim_manager::ShimId;
        let effective_id = ShimId(format!("chromium:{session_id}"));
        if !self.shim.is_registered(&effective_id) {
            return Err(LoomError::new(
                LoomErrorCode::SurfaceTrap,
                "no active page target — navigate to the call before web.inject_audio",
            ));
        }
        let shim_session_id = self.shim.shim_session_id_for(session_id);
        self.shim
            .send_inject_audio(effective_id, shim_session_id, 0, audio_bytes, await_playout)
            .await
    }

    /// video-capture: stop the active recording, write the encoded `.webm` to
    /// the content store, and return the content hash + metadata. The `.webm`
    /// bytes are non-deterministic and live OUTSIDE the manifest hash chain
    /// (only this hash is recorded), so recording never affects replay-equality.
    pub async fn stop_recording(&self, session_id: &str) -> Result<ScreencastResult, LoomError> {
        use crate::shim_manager::ShimId;
        let effective_id = ShimId(format!("chromium:{session_id}"));
        if !self.shim.is_registered(&effective_id) {
            return Err(LoomError::new(
                LoomErrorCode::SurfaceTrap,
                "no active recording (no page target)",
            ));
        }
        let shim_session_id = self.shim.shim_session_id_for(session_id);
        let outcome = self
            .shim
            .send_stop_recording(effective_id, shim_session_id, 0)
            .await?;

        // Write the encoded webm to CAS (host-side, like screenshot bytes). On a
        // best-effort failure (encoder unavailable / zero frames) webm_bytes is
        // empty → no hash, and `outcome.error` carries the reason for the receipt.
        let mut error = outcome.error;
        let screencast_after_hash = if outcome.webm_bytes.is_empty() {
            None
        } else {
            match self.core.content_store.put(&outcome.webm_bytes) {
                Ok(cref) => Some(cref.sha256),
                Err(e) => {
                    // The encode SUCCEEDED but the store failed — record an
                    // accurate error so the receipt doesn't claim "no video"
                    // (ship-council: misleading recording_failed receipt).
                    tracing::warn!(session_id = %session_id, error = %e, "screencast CAS put failed");
                    error = Some(format!(
                        "encoded {} frames but CAS store failed: {e}",
                        outcome.frame_count
                    ));
                    None
                }
            }
        };
        Ok(ScreencastResult {
            screencast_after_hash,
            frame_count: outcome.frame_count,
            duration_ms: outcome.duration_ms,
            stop_reason: outcome.stop_reason,
            error,
        })
    }

    /// voice-call-io (task 06): start capturing inbound (remote) WebRTC audio on the
    /// session's active page target. Errors if no page exists yet (navigate + join
    /// the call first). Does NOT spawn Chromium — a capture only makes sense once a
    /// page is live. The daemon has already enforced the `no_determinism` gate.
    pub async fn start_audio_capture(
        &self,
        session_id: &str,
        max_duration_ms: u64,
        max_bytes: u64,
    ) -> Result<(), LoomError> {
        use crate::shim_manager::ShimId;
        let effective_id = ShimId(format!("chromium:{session_id}"));
        if !self.shim.is_registered(&effective_id) {
            return Err(LoomError::new(
                LoomErrorCode::SurfaceTrap,
                "no active page target — navigate before web.start_audio_capture",
            ));
        }
        let shim_session_id = self.shim.shim_session_id_for(session_id);
        self.shim
            .send_start_audio_capture(effective_id, shim_session_id, 0, max_duration_ms, max_bytes)
            .await
    }

    /// voice-call-io (task 06): stop the active capture, write the captured `.wav`
    /// to the content store, and return the content hash + metadata. The WAV bytes
    /// are non-deterministic and live OUTSIDE the manifest hash chain (only this
    /// hash is recorded), so capture never affects replay-equality — exactly like
    /// screencast.
    pub async fn stop_audio_capture(
        &self,
        session_id: &str,
    ) -> Result<AudioCaptureResult, LoomError> {
        use crate::shim_manager::ShimId;
        let effective_id = ShimId(format!("chromium:{session_id}"));
        if !self.shim.is_registered(&effective_id) {
            return Err(LoomError::new(
                LoomErrorCode::SurfaceTrap,
                "no active capture (no page target)",
            ));
        }
        let shim_session_id = self.shim.shim_session_id_for(session_id);
        let outcome = self
            .shim
            .send_stop_audio_capture(effective_id, shim_session_id, 0)
            .await?;

        // Write the WAV to CAS (host-side, like screencast). On empty capture there
        // is no hash; `outcome.error` carries the reason for the receipt.
        let mut error = outcome.error;
        let audio_after_hash = if outcome.wav_bytes.is_empty() {
            None
        } else if outcome.wav_bytes.len() as u64 > HOST_MAX_CAPTURE_BYTES {
            // [C1/M14] Host cap re-check: never trust the shim's byte count. A
            // compromised/buggy shim returning an over-cap payload must not push
            // unbounded bytes into CAS — reject with a typed error, no store.
            tracing::warn!(
                session_id = %session_id,
                bytes = outcome.wav_bytes.len(),
                "audio capture exceeded host byte ceiling; rejecting (shim cap bypass?)"
            );
            error = Some(format!(
                "captured audio {} bytes exceeds host ceiling {HOST_MAX_CAPTURE_BYTES}; rejected",
                outcome.wav_bytes.len()
            ));
            None
        } else {
            match self.core.content_store.put(&outcome.wav_bytes) {
                Ok(cref) => Some(cref.sha256),
                Err(e) => {
                    // Capture SUCCEEDED but the store failed — record an accurate
                    // error so the receipt doesn't silently claim "no audio".
                    tracing::warn!(session_id = %session_id, error = %e, "audio capture CAS put failed");
                    error = Some(format!("captured audio but CAS store failed: {e}"));
                    None
                }
            }
        };
        Ok(AudioCaptureResult {
            audio_after_hash,
            sample_count: outcome.sample_count,
            duration_ms: outcome.duration_ms,
            dropped_frames: outcome.dropped_frames,
            source_sample_rate: outcome.source_sample_rate,
            stop_reason: outcome.stop_reason,
            error,
        })
    }

    /// cdp-trusted-input: `web.type` DEFAULT (`mode:"fill"`). Focus the element
    /// and drive the value through CDP `Input.insertText` (Playwright `fill()`
    /// semantics) so React/react-hook-form `onChange` fires AND the value is
    /// treated as genuinely user-entered. Host-side intercept (like recording) —
    /// does NOT run the WASM guest. A missing shim (no navigate yet) is a clear
    /// error, not a silent no-op.
    pub async fn type_fill(
        &self,
        session_id: &str,
        selector: &str,
        text: &str,
        budget_ms: u64,
    ) -> Result<crate::shim_manager::InputDispatchOutcome, LoomError> {
        use crate::shim_manager::ShimId;
        let effective_id = ShimId(format!("chromium:{session_id}"));
        if !self.shim.is_registered(&effective_id) {
            return Err(LoomError::new(
                LoomErrorCode::SurfaceTrap,
                "no active page target — navigate before web.type",
            ));
        }
        let shim_session_id = self.shim.shim_session_id_for(session_id);
        let result = self
            .shim
            .send_type_fill(
                effective_id,
                shim_session_id,
                0,
                selector.to_string(),
                text.to_string(),
                budget_ms,
            )
            .await;
        // Redacted observability (F7): log selector + char COUNT + outcome —
        // NEVER the typed text (could be a password).
        match &result {
            Ok(o) => {
                tracing::debug!(session_id = %session_id, selector = %selector, chars = text.chars().count(), outcome = ?o, "web.type fill dispatched")
            }
            Err(e) => {
                tracing::warn!(session_id = %session_id, selector = %selector, error = %e, "web.type fill failed")
            }
        }
        result
    }

    /// cdp-trusted-input: `web.type mode:keystrokes`. Focus the element and send
    /// real per-character `Input.dispatchKeyEvent` frames (`isTrusted:true`).
    /// Host-side intercept (like recording) — does NOT run the WASM guest. A
    /// missing shim (no navigate yet) is a clear error, not a silent no-op.
    pub async fn type_keystrokes(
        &self,
        session_id: &str,
        selector: &str,
        text: &str,
        budget_ms: u64,
    ) -> Result<crate::shim_manager::InputDispatchOutcome, LoomError> {
        use crate::shim_manager::ShimId;
        let effective_id = ShimId(format!("chromium:{session_id}"));
        if !self.shim.is_registered(&effective_id) {
            return Err(LoomError::new(
                LoomErrorCode::SurfaceTrap,
                "no active page target — navigate before web.type",
            ));
        }
        let shim_session_id = self.shim.shim_session_id_for(session_id);
        let result = self
            .shim
            .send_type_keystrokes(
                effective_id,
                shim_session_id,
                0,
                selector.to_string(),
                text.to_string(),
                budget_ms,
            )
            .await;
        // Redacted observability (F7): log selector + char COUNT + outcome —
        // NEVER the typed text (could be a password).
        match &result {
            Ok(o) => {
                tracing::debug!(session_id = %session_id, selector = %selector, chars = text.chars().count(), outcome = ?o, "web.type keystrokes dispatched")
            }
            Err(e) => {
                tracing::warn!(session_id = %session_id, selector = %selector, error = %e, "web.type keystrokes failed")
            }
        }
        result
    }

    /// cdp-trusted-input: `web.press_key`. Optionally focus `selector`, then
    /// dispatch a named key (+ modifiers) as real `Input.dispatchKeyEvent`.
    pub async fn press_key(
        &self,
        session_id: &str,
        key: &str,
        selector: Option<String>,
        modifiers: Vec<String>,
        budget_ms: u64,
    ) -> Result<crate::shim_manager::InputDispatchOutcome, LoomError> {
        use crate::shim_manager::ShimId;
        let effective_id = ShimId(format!("chromium:{session_id}"));
        if !self.shim.is_registered(&effective_id) {
            return Err(LoomError::new(
                LoomErrorCode::SurfaceTrap,
                "no active page target — navigate before web.press_key",
            ));
        }
        let shim_session_id = self.shim.shim_session_id_for(session_id);
        let has_selector = selector.is_some();
        let n_mods = modifiers.len();
        let result = self
            .shim
            .send_press_key(crate::shim_manager::SendPressKeyParams {
                id: effective_id,
                session_id: shim_session_id,
                target_id: 0,
                key: key.to_string(),
                selector,
                modifiers,
                budget_ms,
            })
            .await;
        // Redacted observability (F7): log structure + outcome — NEVER the key
        // value (a single-char key could be a typed secret).
        match &result {
            Ok(o) => {
                tracing::debug!(session_id = %session_id, has_selector, modifiers = n_mods, outcome = ?o, "web.press_key dispatched")
            }
            Err(e) => tracing::warn!(session_id = %session_id, error = %e, "web.press_key failed"),
        }
        result
    }

    /// cdp-trusted-input: always-trusted `web.click`. Resolve the element hit
    /// point and dispatch a trusted `Input.dispatchMouseEvent` sequence.
    pub async fn trusted_click(
        &self,
        session_id: &str,
        selector: &str,
        budget_ms: u64,
    ) -> Result<crate::shim_manager::InputDispatchOutcome, LoomError> {
        use crate::shim_manager::ShimId;
        let effective_id = ShimId(format!("chromium:{session_id}"));
        if !self.shim.is_registered(&effective_id) {
            return Err(LoomError::new(
                LoomErrorCode::SurfaceTrap,
                "no active page target — navigate before web.click",
            ));
        }
        let shim_session_id = self.shim.shim_session_id_for(session_id);
        let result = self
            .shim
            .send_trusted_click(
                effective_id,
                shim_session_id,
                0,
                selector.to_string(),
                budget_ms,
            )
            .await;
        // Redacted observability (F7): selector is a CSS selector, not secret.
        match &result {
            Ok(o) => {
                tracing::debug!(session_id = %session_id, selector = %selector, outcome = ?o, "web.click trusted dispatched")
            }
            Err(e) => {
                tracing::warn!(session_id = %session_id, selector = %selector, error = %e, "web.click trusted failed")
            }
        }
        result
    }

    /// `web.wait` — poll the locator until it resolves or `timeout_ms` elapses.
    /// Mirrors [`trusted_click`](Self::trusted_click): a registered-target guard,
    /// then the host-side `send_wait` (which reuses the same locator-grammar
    /// resolution, so `web.wait` accepts `css=`/`text=`/`role=`/`frame=`, not just
    /// a bare CSS selector). Returns the typed wait outcome; transport failures
    /// surface as `Err`.
    pub async fn wait(
        &self,
        session_id: &str,
        selector: &str,
        timeout_ms: Option<u64>,
    ) -> Result<crate::shim_manager::WaitResolveOutcome, LoomError> {
        use crate::shim_manager::ShimId;
        let effective_id = ShimId(format!("chromium:{session_id}"));
        if !self.shim.is_registered(&effective_id) {
            return Err(LoomError::new(
                LoomErrorCode::SurfaceTrap,
                "no active page target — navigate before web.wait",
            ));
        }
        let shim_session_id = self.shim.shim_session_id_for(session_id);
        let result = self
            .shim
            .send_wait(
                effective_id,
                shim_session_id,
                0,
                selector.to_string(),
                timeout_ms,
            )
            .await;
        // Redacted observability (F7): selector is a CSS selector, not secret.
        match &result {
            Ok(o) => {
                tracing::debug!(session_id = %session_id, selector = %selector, timeout_ms = ?timeout_ms, outcome = ?o, "web.wait dispatched")
            }
            Err(e) => {
                tracing::warn!(session_id = %session_id, selector = %selector, error = %e, "web.wait failed")
            }
        }
        result
    }
}

/// Result of [`WasmHost::stop_recording`]. `screencast_after_hash` is the CAS
/// SHA-256 of the encoded `.webm` (None on a best-effort encode failure, in
/// which case `error` is set).
#[derive(Debug, Default, Clone)]
pub struct ScreencastResult {
    pub screencast_after_hash: Option<String>,
    pub frame_count: u64,
    pub duration_ms: u64,
    pub stop_reason: String,
    pub error: Option<String>,
}

/// Host-side absolute ceiling on a single captured WAV before it enters CAS —
/// defense-in-depth beyond the shim `Caps` (a compromised/buggy shim cannot force
/// unbounded CAS growth). 64 MiB ≈ 35 min of 16 kHz mono i16, far above any
/// legitimate bounded voice capture.
const HOST_MAX_CAPTURE_BYTES: u64 = 64 * 1024 * 1024;

/// Result of [`WasmHost::stop_audio_capture`]. `audio_after_hash` is the CAS
/// SHA-256 of the captured `.wav` (None on an empty capture, an over-ceiling
/// reject, or a CAS failure — in which case `error` is set). Mirrors
/// [`ScreencastResult`]; the audio bytes never enter the manifest hash chain.
#[derive(Debug, Default, Clone)]
pub struct AudioCaptureResult {
    pub audio_after_hash: Option<String>,
    pub sample_count: u64,
    pub duration_ms: u64,
    pub dropped_frames: u64,
    pub source_sample_rate: u32,
    pub stop_reason: String,
    pub error: Option<String>,
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
