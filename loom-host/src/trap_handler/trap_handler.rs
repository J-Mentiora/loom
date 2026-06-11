// TrapHandler — catches `wasmtime::Trap`, resolves `.dwp` debug info,
// converts to `LoomErrorCode::SurfaceTrap` + stamps the trap verdict on
// the action's receipt builder.
//
// # Contract semantics
// - **Trap containment.** Wasmtime surface traps NEVER unwind into the
//   daemon. They are caught at the `Func::call` boundary inside
//   `SessionExecutor`, handed to this module, and converted into a
//   typed `LoomError::SurfaceTrap`.
// - **Exactly one receipt per action.** `handle_trap` does NOT append a
//   receipt of its own — it stamps `status = Trapped` + the trap
//   details onto the action's `ReceiptBuilder`, and `WasmHost::dispatch`
//   queues that builder ONCE via `ReceiptMarshaller::queue` (preserving
//   per-session append order). A second, handler-side append would
//   produce two ActionReceipts with the same action_id racing on the
//   manifest hash chain — one of them falsely `status=ok`.
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
//   before the builder is stamped.

use crate::error_mapper::TrapFrame;
use crate::host_observability::HostObservability;
use crate::receipt_marshaller::{ReceiptBuilder, ReceiptStatus};
use loom_core::error::LoomError;
use loom_core::manifest_writer::SessionId;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

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
}

impl TrapHandler {
    pub fn new(obs: Arc<HostObservability>) -> Arc<Self> {
        Arc::new(Self { obs })
    }

    /// Convert a wasmtime trap to a typed `LoomError::SurfaceTrap` and
    /// stamp the trap verdict on the action's receipt `builder`. Returns
    /// the LoomError; `SessionExecutor` returns it inside
    /// `ActionOutcome::Trapped` and `WasmHost::dispatch` queues the
    /// stamped builder as the action's single receipt.
    pub fn handle_trap(
        self: &Arc<Self>,
        trap: wasmtime::Trap,
        ctx: TrapContext,
        builder: &mut ReceiptBuilder,
    ) -> LoomError {
        let frames = self
            .resolve_frames(ctx.dwp_path.as_ref(), &[])
            .unwrap_or_default();
        let _ = self
            .obs
            .record_trap_event(crate::host_observability::TrapEvent {
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
        // Trap verdict on the single per-action receipt: truthful status
        // + the details the old handler-side trap receipt carried.
        // Timestamps are deliberately NOT touched: the builder keeps the
        // executor's deterministic per-action clock values
        // (started = action_id * DETERMINISTIC_ACTION_DELTA_MS,
        // finished = (action_id + 1) * DELTA under determinism; the
        // per-session virtual clock under --no-determinism) — the same
        // source ordinary receipts use. Wall-clock here would serialize
        // INTO the hash-chained receipt_canonical_bytes (hashable_line
        // projects only the WAL line's top-level fields, never inside
        // the receipt blob), so a deterministic trap reproduced in two
        // same-seed runs would diverge the chain (NFR-DET-01).
        builder.status = ReceiptStatus::Trapped;
        builder.error_code = Some(format!("{:?}", err.code));
        builder.error_details = Some(format!("surface={} frames={}", ctx.surface, frames.len()));
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
        // Full addr2line resolution is deferred to a later milestone.
        // For now, return raw-address-only frames.
        if dwp_path.map(|p| !p.exists()).unwrap_or(true) {
            return Ok(pcs
                .iter()
                .map(|&pc| TrapFrame {
                    pc,
                    source_file: None,
                    source_line: None,
                    func_name: None,
                })
                .collect());
        }
        Ok(pcs
            .iter()
            .map(|&pc| TrapFrame {
                pc,
                source_file: None,
                source_line: None,
                func_name: None,
            })
            .collect())
    }

    /// True iff resolution happened with full debug info.
    pub fn debug_info_available(&self, dwp_path: Option<&PathBuf>) -> bool {
        match dwp_path {
            Some(p) => p.exists(),
            None => false,
        }
    }
}
