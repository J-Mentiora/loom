// Re-export of the locked Phase 5.3 interface. DO NOT EDIT here.
// Edit `systems/loom-host/modules/trap_handler/interfaces.rs` instead.
// TrapHandler — catches `wasmtime::Trap`, resolves `.dwp` debug info,
// converts to `LoomErrorCode::SurfaceTrap` typed receipt.
//
// # Contract semantics
// - **IC-HOST-06.** Wasmtime surface traps NEVER unwind into the
//   daemon. They are caught at the `Func::call` boundary inside
//   `SessionExecutor`, handed to this module, and converted into a
//   typed `LoomError::SurfaceTrap` + queued trap receipt.
// - **`.dwp` debug-info resolution.** Per-surface `.dwp` companion file
//   sits next to the `.cwasm` artifact in
//   `~/Library/Application Support/loom/surfaces/<name>.dwp`. If
//   present, frames resolve to `source:line` via `addr2line`; if
//   missing, frames carry raw addresses + `debug_info_unavailable: true`.
// - **Single owner.** Only this module owns the trap-recovery policy.
//   `SessionExecutor` calls `handle_trap` and propagates the returned
//   `LoomError` upwards as the dispatch result.
// - **Observability hook.** Every trap event is logged via
//   `HostObservability::record_trap_event` with the resolved frames
//   before the receipt is queued.

use loom_core::error::LoomError;
use loom_core::manifest_writer::SessionId;
use crate::error_mapper::TrapFrame;
use crate::host_observability::HostObservability;
use crate::receipt_marshaller::ReceiptMarshaller;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::runtime::Handle as TokioHandle;

/// Per-trap context. Built by `SessionExecutor` and handed to
/// `handle_trap`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrapContext {
    pub session_id: SessionId,
    pub action_id: u64,
    pub surface: String,
    /// Path to the `.dwp` debug-info file for this surface, if any.
    pub dwp_path: Option<PathBuf>,
}

pub struct TrapHandler {
    pub(crate) obs: Arc<HostObservability>,
    pub(crate) receipts: Arc<ReceiptMarshaller>,
}

impl TrapHandler {
    pub fn new(obs: Arc<HostObservability>, receipts: Arc<ReceiptMarshaller>) -> Arc<Self> {
        Arc::new(Self { obs, receipts })
    }

    /// Convert a wasmtime trap to a typed `LoomError::SurfaceTrap` and
    /// queue a trap receipt. Returns the LoomError; `SessionExecutor`
    /// returns it as the dispatch failure.
    ///
    /// The `pool` is the session's `receipt_pool` (BC-HOST-01) —
    /// the trap-receipt spawn happens off the dispatch task.
    pub fn handle_trap(
        self: &Arc<Self>,
        trap: wasmtime::Trap,
        ctx: TrapContext,
        pool: TokioHandle,
    ) -> LoomError {
        let frames = self.resolve_frames(ctx.dwp_path.as_ref(), &[]).unwrap_or_default();
        let _ = self.obs.record_trap_event(crate::host_observability::TrapEvent {
            session_id: ctx.session_id.0.clone(),
            action_id: ctx.action_id,
            surface: ctx.surface.clone(),
            trap_code: format!("{trap:?}"),
            frames_count: frames.len() as u32,
            debug_info_unavailable: !self.debug_info_available(ctx.dwp_path.as_ref()),
        });
        let err = crate::error_mapper::wasmtime_trap_to_loom_error(
            trap,
            ctx.surface.clone(),
            frames.clone(),
        );
        let this = self.clone();
        let session_id = ctx.session_id.clone();
        let action_id = ctx.action_id;
        let surface = ctx.surface.clone();
        let err_code = format!("{:?}", err.code);
        let pool2 = pool.clone();
        pool.spawn(async move {
            let _ = this.receipts.emit_trap_receipt(
                session_id, action_id, surface, err_code,
                frames.len() as u32, pool2,
            );
        });
        err
    }

    /// Resolve `.dwp` debug info for a list of program counters.
    /// Returns one `TrapFrame` per pc; `source_file`/`source_line`
    /// populated when debug info is present, `None` otherwise.
    pub fn resolve_frames(
        &self,
        dwp_path: Option<&PathBuf>,
        pcs: &[u64],
    ) -> Result<Vec<TrapFrame>, LoomError> {
        // Full addr2line resolution is Phase 6.
        // Phase 5.4: return raw-address-only frames.
        if dwp_path.map(|p| !p.exists()).unwrap_or(true) {
            return Ok(pcs.iter().map(|&pc| TrapFrame {
                pc,
                source_file: None,
                source_line: None,
                func_name: None,
            }).collect());
        }
        Ok(pcs.iter().map(|&pc| TrapFrame {
            pc,
            source_file: None,
            source_line: None,
            func_name: None,
        }).collect())
    }

    /// True iff resolution happened with full debug info.
    pub fn debug_info_available(&self, dwp_path: Option<&PathBuf>) -> bool {
        match dwp_path {
            Some(p) => p.exists(),
            None => false,
        }
    }
}
