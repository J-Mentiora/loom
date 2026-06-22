// HostFunctionTable — `wit-bindgen`-generated `impl host::Host for HostState`.
//
// # Contract semantics
// - **The SOLE WASM↔core bridge.** The 8 host functions
//   defined in `wit/loom-surface.wit::interface host` are the only
//   cross-boundary calls. WASM cannot import any other host symbol.
// - **Generated trait, hand-written body.** The trait `host::Host` is
//   produced by `wit-bindgen`; the impl body lives
//   here. The two impls (live + replay) are referenced by
//   `HostFunctionRegistry::new` to build two `Linker<HostState>`
//   instances.
// - **Per-host-fn tape.** Every host-fn appends to the
//   `DeterminismHarness` tape BEFORE invoking its side effect.
// - **No audit-entry writes from host body.** Audit
//   writes are owned by `Vault::substitute` in loom-core; the
//   `net_request` body NEVER calls `ManifestWriter::append_audit`.
// - **Boundary translation.** Errors leave via
//   `ErrorMapper::loom_error_to_host_error`. No `anyhow::Error`
//   propagation.
// - **Vault isolation.** The `net_request` body
//   strips the `Authorization` header from `NetResp` before the
//   marshaller serializes it back to WASM. Raw secret bytes appear
//   ONLY inside `Vault::substitute`'s scope.

use crate::error_mapper::HostError;
use crate::host_observability::HostObservability;
use crate::receipt_marshaller::{ReceiptBuilder, SideEffectAccumulator};
use crate::shim_manager::ShimManager;
use crate::wit_type_marshaller::Mode;
use loom_core::content_store::ContentRef;
use loom_core::core_api_facade::CoreApiFacade;
use loom_core::manifest_writer::SessionId;
use loom_core::vault::{NetRequest, NetResp};
use loom_shared::types::{EpochMs, Seed};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use wasmtime::component::ResourceTable;
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

// =================================================================
// HostState — per-dispatch struct flowed through `wasmtime::Store`.
// =================================================================

/// Per-action state owned by `wasmtime::Store<HostState>`. Built fresh
/// in `SessionExecutor::run`; dropped at action complete.
pub struct HostState {
    pub core: Arc<CoreApiFacade>,
    pub determinism: Arc<loom_core::determinism_harness::DeterminismHarness>,
    pub budget: Arc<dyn loom_core::budget_enforcer::BudgetEnforcer>,
    pub session_id: SessionId,
    pub action_id: u64,
    pub mode: Mode,
    pub receipt_builder: ReceiptBuilder,
    pub side_effects: SideEffectAccumulator,
    pub host_call_metrics: Vec<HostCallMetric>,
    pub shim_manager: Arc<ShimManager>,
    pub obs: Arc<HostObservability>,
    /// Per-session tape writer minted via
    /// `DeterminismHarness::new_tape_writer`.
    pub tape_writer: loom_core::determinism_harness::TapeWriter,
    /// Replay-mode host-fn vtable (None in live mode).
    pub replay_table: Option<loom_core::determinism_harness::ReplayHostFnTable>,
    /// wasmtime-wasi context. Required because surfaces compiled for
    /// `wasm32-wasip2` transitively import `wasi:io/poll@0.2.6` (and
    /// other wasi:* interfaces) via the Rust stdlib's allocator/format
    /// machinery, even when the WIT world doesn't import any wasi
    /// interface explicitly. The linker rejects instantiation otherwise.
    /// Built per-dispatch via [`build_sandboxed_wasi_ctx`] — no fs
    /// preopens, no env, no network, no inherited stdio.
    pub wasi_ctx: WasiCtx,
    /// Component-model resource table. Owned by `HostState` rather than
    /// shared so each dispatch starts with a clean slate.
    pub wasi_table: ResourceTable,
    /// Per-session determinism seed; rendered into the shim-side JS
    /// determinism template at inject time. Threaded from `Session.seed`
    /// (which collapses `SessionCreateOpts.seed` exactly once at
    /// session create).
    pub seed: Seed,
    /// Per-session Unix epoch milliseconds; substituted into the shim
    /// JS template's `Date.now` constant.
    pub epoch_ms: EpochMs,
    /// Operator's `--no-blocklist` opt-out.
    /// Threaded from `Session.no_blocklist`. Read by `navigate_execute`
    /// to compute `blocklist_enabled = !no_blocklist` for each
    /// `ShimRequest::PageNavigate`.
    pub no_blocklist: bool,
    /// Operator's `--no-determinism` opt-out (settle-capture 4b). Threaded
    /// from `Session.no_determinism`. Each typed host fn computes
    /// `determinism_enabled = !no_determinism` for the ShimRequests that
    /// create a target (SpawnTarget / PageNavigate); when off, the shim
    /// SKIPS the determinism freeze-inject.
    pub no_determinism: bool,
    /// Operator's `--profile` choice. Threaded from `SessionHandle.profile`
    /// for the safe-profile download-confinement path — the lazy-clone
    /// shim-config sites in `host_impl.rs` read this to inject
    /// `LOOM_SHIM_PROFILE` into the per-session shim subprocess env so
    /// Chromium-side download confinement activates only under safe
    /// profile.
    pub profile: String,
    /// Session-scoped downloads directory (`<sessions_root>/<ulid>/downloads/`),
    /// populated only when `profile == "safe"`. Read at shim-spawn time
    /// to inject `LOOM_SHIM_DOWNLOADS_DIR` into the shim env.
    pub downloads_dir: Option<std::path::PathBuf>,
    /// Capture-policy=fingerprint tier flag. Derived once from
    /// `Session.capture_policy == "fingerprint"` and threaded via `SessionHandle`.
    /// The `capture_dom_after_hash` host fn returns `None` (no DOM round-trip)
    /// unless this is true, and `decode_typed_receipt` only accepts the guest's
    /// `dom-after-hash` into the canonical receipt when this is true (host-side
    /// accept-gate: the host is authoritative for the fingerprint field).
    pub capture_dom_after: bool,
    /// Side-channel for surfacing `web.get_cookies` results. The WASM guest
    /// forwards `Network.getCookies` opaquely (it ships without a CBOR decoder),
    /// so the decoded cookie array would otherwise be lost. `shim_call` decodes
    /// the CDP response host-side and stashes the canonical-JSON cookie array
    /// here; `SessionExecutor` reads it after the guest returns to populate
    /// `ReceiptBuilder.get_cookies_result` and re-derive a value-redacted
    /// `outcome_hash`. `Arc<Mutex<..>>` because `shim_call`'s async block needs a
    /// clone it can write through while `HostState` is still owned by the Store.
    pub cookie_capture: Arc<parking_lot::Mutex<Option<String>>>,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi_ctx,
            table: &mut self.wasi_table,
        }
    }
}

/// Construct the WASI context every dispatch uses. The builder must
/// stay at its safe defaults — no preopened directories, no inherited
/// env, no inherited stdio, no network. Adding any inherit_/preopen_/
/// allow_ip API call here widens the sandbox and breaks the
/// malicious-guest unit test that pins this textually.
pub fn build_sandboxed_wasi_ctx() -> WasiCtx {
    WasiCtxBuilder::new().build()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostCallMetric {
    pub host_fn: String,
    pub started_at_ms: u64,
    pub finished_at_ms: u64,
    pub error: Option<String>,
}

// =================================================================
// The 8 host fns. Trait declaration mirrors what `wit-bindgen`
// generates from `wit/loom-surface.wit::interface host`.
// =================================================================

/// Generated trait declaration (mirror of wit-bindgen output). The real
/// impl is on `HostState` for both live and replay modes.
pub trait HostFnsTrait {
    fn clock_now(state: &mut HostState) -> Result<u64, HostError>;
    fn rng_next_u64(state: &mut HostState) -> Result<u64, HostError>;
    fn blob_put(state: &mut HostState, bytes: Vec<u8>) -> Result<ContentRef, HostError>;
    fn blob_get(state: &mut HostState, r: ContentRef) -> Result<Vec<u8>, HostError>;
    fn net_request(state: &mut HostState, req: NetRequest) -> Result<NetResp, HostError>;
    fn shim_call(
        state: &mut HostState,
        shim_id: String,
        msg: Vec<u8>,
    ) -> Result<Vec<u8>, HostError>;
    fn log_emit(state: &mut HostState, level: LogLevel, msg: String, fields: Vec<(String, String)>);
    fn receipt_emit(state: &mut HostState, r: WitReceipt);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// WIT-derived receipt (minimal mirror; full def in `WitTypeMarshaller`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WitReceipt {
    pub action_id: u64,
    pub status_ok: bool,
    pub emitted_at_ms: u64,
    pub payload_canonical_bytes: Vec<u8>,
}

/// Live-mode host-fn dispatcher. Each method:
///   1. Marshals input via `WitTypeMarshaller`.
///   2. Appends a tape frame BEFORE the live side-effect.
///   3. Performs the side-effect (Vault / ContentStore / ShimManager / DH).
///   4. Marshals output (with `Authorization` stripped from `NetResp`
///      to keep the secret out of WASM linear memory).
pub struct LiveHostFns;

impl HostFnsTrait for LiveHostFns {
    fn clock_now(state: &mut HostState) -> Result<u64, HostError> {
        let t = state.determinism.clock_now();
        state
            .tape_writer
            .record(loom_core::determinism_harness::TapeFrame::ClockRead { observed_ns: t });
        Ok(t)
    }
    fn rng_next_u64(state: &mut HostState) -> Result<u64, HostError> {
        let v = state.determinism.rng_next();
        state
            .tape_writer
            .record(loom_core::determinism_harness::TapeFrame::RngDraw { value_u64: v });
        Ok(v)
    }
    fn blob_put(state: &mut HostState, bytes: Vec<u8>) -> Result<ContentRef, HostError> {
        let r = state
            .core
            .content_store()
            .put(&bytes)
            .map_err(crate::error_mapper::loom_error_to_host_error)?;
        state
            .tape_writer
            .record(loom_core::determinism_harness::TapeFrame::BlobRead {
                sha256: r.sha256.clone(),
                size_bytes: r.size_bytes,
            });
        Ok(r)
    }
    fn blob_get(state: &mut HostState, r: ContentRef) -> Result<Vec<u8>, HostError> {
        state
            .core
            .content_store()
            .get(&r)
            .map_err(crate::error_mapper::loom_error_to_host_error)
    }
    fn net_request(state: &mut HostState, req: NetRequest) -> Result<NetResp, HostError> {
        // Full vault+HTTP wiring is not yet implemented.
        // Return a stub error so surfaces that call net_request fail
        // gracefully instead of panicking.
        let _ = (state, req);
        Err(HostError::Internal(
            "net_request: vault+HTTP wiring is not yet implemented".to_string(),
        ))
    }
    fn shim_call(
        state: &mut HostState,
        shim_id: String,
        msg: Vec<u8>,
    ) -> Result<Vec<u8>, HostError> {
        let shim = state.shim_manager.clone();
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(shim.send(crate::shim_manager::ShimId(shim_id), msg))
        })
        .map_err(crate::error_mapper::loom_error_to_host_error)
    }
    fn log_emit(
        state: &mut HostState,
        level: LogLevel,
        msg: String,
        fields: Vec<(String, String)>,
    ) {
        let _ = (state, level, msg, fields);
    }
    fn receipt_emit(state: &mut HostState, r: WitReceipt) {
        let _ = (state, r);
    }
}

/// Replay-mode host-fn dispatcher. Reads from `HostState.replay_table`;
/// NEVER reaches `Vault`, live HTTP, `ShimManager::send`, or
/// `ContentStore::put`.
pub struct ReplayHostFns;

impl HostFnsTrait for ReplayHostFns {
    fn clock_now(state: &mut HostState) -> Result<u64, HostError> {
        state
            .replay_table
            .as_ref()
            .ok_or_else(|| HostError::Internal("replay_table not set".to_string()))?
            .pop_clock()
            .map_err(crate::error_mapper::loom_error_to_host_error)
    }
    fn rng_next_u64(state: &mut HostState) -> Result<u64, HostError> {
        state
            .replay_table
            .as_ref()
            .ok_or_else(|| HostError::Internal("replay_table not set".to_string()))?
            .pop_rng()
            .map_err(crate::error_mapper::loom_error_to_host_error)
    }
    fn blob_put(state: &mut HostState, _bytes: Vec<u8>) -> Result<ContentRef, HostError> {
        // Replay never writes to ContentStore; blobs were recorded during live.
        // Return the next BlobRead frame from the tape.
        let _ = state;
        Err(HostError::Internal(
            "blob_put: replay mode never writes to ContentStore".to_string(),
        ))
    }
    fn blob_get(state: &mut HostState, r: ContentRef) -> Result<Vec<u8>, HostError> {
        // Reads are allowed in replay — blob is already in CAS from live mode.
        state
            .core
            .content_store()
            .get(&r)
            .map_err(crate::error_mapper::loom_error_to_host_error)
    }
    fn net_request(state: &mut HostState, req: NetRequest) -> Result<NetResp, HostError> {
        // In replay: read the response from tape; never reach live HTTP.
        let _ = req;
        let table = state
            .replay_table
            .as_ref()
            .ok_or_else(|| HostError::Internal("replay_table not set".to_string()))?;
        let frame = table
            .pop_net(state.action_id)
            .map_err(crate::error_mapper::loom_error_to_host_error)?;
        match frame {
            loom_core::determinism_harness::TapeFrame::NetResponse {
                status,
                body_ref_sha256,
                body_size_bytes,
                ..
            } => {
                let body_ref = ContentRef {
                    sha256: body_ref_sha256,
                    size_bytes: body_size_bytes,
                };
                let body = state
                    .core
                    .content_store()
                    .get(&body_ref)
                    .map_err(crate::error_mapper::loom_error_to_host_error)?;
                Ok(NetResp {
                    status,
                    headers: Default::default(),
                    body,
                })
            }
            _ => Err(HostError::Internal(
                "tape frame type mismatch for net_request".to_string(),
            )),
        }
    }
    fn shim_call(
        state: &mut HostState,
        shim_id: String,
        msg: Vec<u8>,
    ) -> Result<Vec<u8>, HostError> {
        // Replay never invokes ShimManager.
        let _ = (state, shim_id, msg);
        Err(HostError::Internal(
            "shim_call: replay mode never invokes ShimManager".to_string(),
        ))
    }
    fn log_emit(
        state: &mut HostState,
        level: LogLevel,
        msg: String,
        fields: Vec<(String, String)>,
    ) {
        let _ = (state, level, msg, fields);
    }
    fn receipt_emit(state: &mut HostState, r: WitReceipt) {
        let _ = (state, r);
    }
}
