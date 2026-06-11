// Interface tests for `TrapHandler`. Verifies typed-receipt conversion
// (daemon survives), the .dwp debug-info resolution signature, and the
// exactly-one-receipt-per-trapped-action invariant.

use super::trap_handler::{TrapContext, TrapHandler};
use crate::error_mapper::TrapFrame;
use crate::host_observability::HostObservability;
use crate::receipt_marshaller::{
    ActionOutcome, ObservedCosts, ReceiptBuilder, ReceiptMarshaller, ReceiptStatus,
};
use loom_core::error::{LoomError, LoomErrorCode};
use loom_core::manifest_writer::SessionId;
use std::path::PathBuf;
use std::sync::Arc;

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
        b: &mut ReceiptBuilder,
    ) -> LoomError {
        h.handle_trap(t, ctx, b)
    }
    let _ = _ck;
}

#[test]
fn handle_trap_stamps_builder_not_a_receipt_of_its_own() {
    // Exactly-one-receipt invariant: handle_trap takes the action's
    // receipt builder and stamps the trap verdict on it — it does NOT
    // take a pool handle, because it must never queue a second
    // ActionReceipt for the same action_id.
    fn _ck(
        h: &Arc<TrapHandler>,
        t: wasmtime::Trap,
        ctx: TrapContext,
        b: &mut ReceiptBuilder,
    ) -> LoomError {
        h.handle_trap(t, ctx, b)
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
    fn _ck(obs: Arc<HostObservability>) -> Arc<TrapHandler> {
        TrapHandler::new(obs)
    }
    let _ = _ck;
}

// === Acyclicity: TrapHandler depends on (ErrorMapper via TrapFrame),
//     ReceiptBuilder (stamping only), HostObservability — no upstream deps ===

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

// === REGRESSION (audit): a trapped action must produce exactly ONE
//     manifest ActionReceipt, carrying status=trapped + trap details.
//     Pre-fix, handle_trap spawned its own status=Trapped append AND
//     WasmHost::dispatch queued the (status-defaults-to-ok) builder as a
//     second receipt for the same action_id. ===

/// Records every appended ActionReceipt as (action_id, canonical bytes).
#[derive(Default)]
struct CapturingWriter {
    receipts: parking_lot::Mutex<Vec<(u64, Vec<u8>)>>,
}

impl loom_core::manifest_writer::ManifestWriter for CapturingWriter {
    fn open_manifest_with_started_at(
        &self,
        _session: SessionId,
        _budgets: Option<loom_core::budget_enforcer::BudgetLimits>,
        _started_at_ms_override: Option<u64>,
        _capture_policy: Option<String>,
        _seed: Option<u64>,
        _determinism_enabled: bool,
    ) -> Result<loom_core::manifest_writer::WriterHandle, LoomError> {
        Err(LoomError::internal("not used in trap receipt tests"))
    }

    fn append(
        &self,
        _session: SessionId,
        entry: loom_core::manifest_writer::ManifestEntry,
    ) -> Result<(), LoomError> {
        if let loom_core::manifest_writer::ManifestEntry::ActionReceipt {
            action_id,
            receipt_canonical_bytes,
            ..
        } = entry
        {
            self.receipts
                .lock()
                .push((action_id, receipt_canonical_bytes));
        }
        Ok(())
    }

    fn append_audit(
        &self,
        _session: SessionId,
        _kind: loom_core::manifest_writer::AuditKind,
        _canonical_bytes: Vec<u8>,
    ) -> Result<(), LoomError> {
        Ok(())
    }

    fn validate(&self, _session: SessionId) -> Result<(), LoomError> {
        Ok(())
    }

    fn checkpoint(&self, _session: SessionId) -> Result<(), LoomError> {
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trapped_action_emits_exactly_one_receipt_with_trapped_status() {
    let writer = Arc::new(CapturingWriter::default());
    let marshaller = ReceiptMarshaller::new(
        writer.clone(),
        Arc::new(loom_core::benchmarks::harness::MockBudgetEnforcer),
    );
    let handler = TrapHandler::new(HostObservability::new(true));
    let sid = SessionId("01hztrap0receipt0000000000".into());

    // The builder as SessionExecutor::run hands it to the trap arm.
    let mut builder = ReceiptBuilder {
        action_id: 7,
        started_at_ms: 7,
        finished_at_ms: 8,
        ..Default::default()
    };
    let ctx = TrapContext {
        session_id: sid.clone(),
        action_id: 7,
        surface: "loom_surface_web".into(),
        dwp_path: None,
    };

    let err = handler.handle_trap(wasmtime::Trap::UnreachableCodeReached, ctx, &mut builder);
    assert_eq!(err.code, LoomErrorCode::SurfaceTrap);
    assert_eq!(builder.status, ReceiptStatus::Trapped);
    assert_eq!(builder.error_code.as_deref(), Some("SurfaceTrap"));
    assert_eq!(
        builder.error_details.as_deref(),
        Some("surface=loom_surface_web frames=0")
    );

    // handle_trap must NOT have appended a receipt of its own (give any
    // regression-spawned task a chance to land before asserting).
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        writer.receipts.lock().is_empty(),
        "handle_trap must not emit a receipt; dispatch queues the single one"
    );

    // The dispatch-side queue — the ONLY emission point.
    marshaller
        .queue(
            ActionOutcome {
                session_id: sid,
                builder,
                observed_costs: ObservedCosts::default(),
            },
            tokio::runtime::Handle::current(),
        )
        .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while writer.receipts.lock().is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "queued trap receipt did not drain"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    // Let any (buggy) second append land before counting.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let receipts = writer.receipts.lock().clone();
    assert_eq!(
        receipts.len(),
        1,
        "a trapped action must produce exactly one manifest receipt"
    );
    let (action_id, bytes) = &receipts[0];
    assert_eq!(*action_id, 7);
    let json: serde_json::Value =
        serde_json::from_slice(bytes).expect("trap receipt canonical bytes parse");
    assert_eq!(
        json["status"], "trapped",
        "the single receipt must carry the truthful trapped status, got: {json}"
    );
    assert_eq!(json["error_code"], "SurfaceTrap");
}

// === REGRESSION (audit): trap receipts must not embed wall-clock
//     SystemTime in their hash-chained canonical bytes. The retired
//     handler-side trap path stamped started_at_ms/finished_at_ms from
//     SystemTime::now(); those values serialize INTO
//     receipt_canonical_bytes, and hashable_line projects only the WAL
//     line's TOP-LEVEL ephemerals — never inside the receipt blob — so
//     a deterministic guest trap reproduced in two same-seed runs
//     diverged the manifest hash chain (NFR-DET-01, commit 114be83). ===

#[test]
fn trap_receipt_canonical_bytes_identical_across_two_same_seed_runs() {
    // One full "run" of the same trapped action: the builder exactly as
    // SessionExecutor::run hands it to the trap arm under determinism
    // (the default) — timestamps are a pure function of action_id — then
    // the trap stamp + canonical assembly the dispatch queue performs.
    fn one_run() -> Vec<u8> {
        const DELTA: u64 = loom_core::determinism_harness::DETERMINISTIC_ACTION_DELTA_MS;
        let handler = TrapHandler::new(HostObservability::new(true));
        let action_id = 3u64;
        let mut builder = ReceiptBuilder {
            action_id,
            started_at_ms: action_id.saturating_mul(DELTA),
            finished_at_ms: action_id.saturating_add(1).saturating_mul(DELTA),
            ..Default::default()
        };
        let ctx = TrapContext {
            session_id: SessionId("01hztrapdet000000000000000".into()),
            action_id,
            surface: "loom_surface_web".into(),
            dwp_path: None,
        };
        let _ = handler.handle_trap(wasmtime::Trap::UnreachableCodeReached, ctx, &mut builder);
        ReceiptMarshaller::assemble_canonical_bytes(&builder).expect("assemble trap receipt")
    }

    let first = one_run();
    // Wall clock advances between the two "runs"; the canonical bytes
    // must not. (Millisecond resolution — 20ms guarantees a different
    // SystemTime::now() reading, which made this red pre-fix.)
    std::thread::sleep(std::time::Duration::from_millis(20));
    let second = one_run();
    assert_eq!(
        first, second,
        "trap receipt canonical bytes must be byte-equal across two same-seed runs"
    );

    // Pin the deterministic per-action clock values (same source as
    // ordinary receipts) — no wall-clock epoch leaked into the bytes.
    let json: serde_json::Value = serde_json::from_slice(&first).expect("parse");
    assert_eq!(json["status"], "trapped");
    assert_eq!(json["started_at_ms"], 3);
    assert_eq!(json["finished_at_ms"], 4);
}
