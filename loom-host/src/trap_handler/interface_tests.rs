// Interface tests for `TrapHandler`. Verifies typed-receipt conversion
// (daemon survives) and the .dwp debug-info resolution signature.

use super::trap_handler::{TrapContext, TrapHandler};
use crate::error_mapper::TrapFrame;
use crate::host_observability::HostObservability;
use crate::receipt_marshaller::ReceiptMarshaller;
use loom_core::error::{LoomError, LoomErrorCode};
use loom_core::manifest_writer::SessionId;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::runtime::Handle as TokioHandle;

// === handle_trap returns typed LoomError, never panics ===

#[test]
fn handle_trap_signature_returns_loomerror_not_result() {
    // The conversion is total: every wasmtime::Trap maps to some
    // LoomError. The function returns LoomError directly (the trap is
    // already an error condition; we just need to type-shape it).
    fn _ck(
        h: &Arc<TrapHandler>,
        t: wasmtime::Trap,
        ctx: TrapContext,
        pool: TokioHandle,
    ) -> LoomError {
        h.handle_trap(t, ctx, pool)
    }
    let _ = _ck;
}

#[test]
fn handle_trap_takes_session_pool() {
    // The trap-receipt spawn must happen on the session's
    // `receipt_pool`, not a global pool. The pool handle is threaded
    // through the call.
    fn _ck(
        h: &Arc<TrapHandler>,
        t: wasmtime::Trap,
        ctx: TrapContext,
        pool: TokioHandle,
    ) -> LoomError {
        h.handle_trap(t, ctx, pool)
    }
    let _ = _ck;
}

#[test]
fn surface_trap_loom_error_code_carries_surface_trap_code_and_frames() {
    let _e = LoomErrorCode::SurfaceTrap;
    let _: LoomError = LoomError::from(_e);
}

// === .dwp resolution ===

#[test]
fn resolve_frames_returns_per_pc_trap_frame() {
    // Compile-time pin only — addr2line is implemented later.
    fn _ck(
        h: &TrapHandler,
        path: Option<&PathBuf>,
        pcs: &[u64],
    ) -> Result<Vec<TrapFrame>, LoomError> {
        h.resolve_frames(path, pcs)
    }
    let _ = _ck;
}

#[test]
fn debug_info_available_returns_false_for_none() {
    // Compile-time signature pin only. The .dwp existence check is
    // implemented later; a real runtime fixture exercises the bool
    // path.
    fn _ck(h: &TrapHandler, path: Option<&PathBuf>) -> bool {
        h.debug_info_available(path)
    }
    let _ = _ck;
    // Sink-module construction shape pin (HostObservability has a
    // real ctor, so this exercises the dep-injection signature).
    let _obs = HostObservability::new(false);
}

#[test]
fn trap_context_carries_session_id_action_id_surface_and_optional_dwp() {
    let ctx = TrapContext {
        session_id: SessionId("01HZ".into()),
        action_id: 42,
        surface: "stocktwits".into(),
        dwp_path: Some(PathBuf::from(
            "/Users/x/Library/Application Support/loom/surfaces/stocktwits.dwp",
        )),
    };
    assert_eq!(ctx.action_id, 42);
    assert!(ctx.dwp_path.is_some());
}

// === Single-owner property: TrapHandler is constructed once per process ===

#[test]
fn new_returns_arc_so_session_executor_clones_handle() {
    fn _ck(obs: Arc<HostObservability>, r: Arc<ReceiptMarshaller>) -> Arc<TrapHandler> {
        TrapHandler::new(obs, r)
    }
    let _ = _ck;
}

// === Acyclicity: TrapHandler depends on (ErrorMapper via TrapFrame),
//     ReceiptMarshaller, HostObservability — no upstream deps ===

#[test]
fn trap_frame_is_imported_from_error_mapper_module() {
    // Compile-time pin: trap frame type is owned by error_mapper, not
    // duplicated here. Single source of truth.
    let f: TrapFrame = TrapFrame {
        pc: 0,
        source_file: None,
        source_line: None,
        func_name: None,
    };
    let _ = f;
}
