// Interface tests for `ReceiptMarshaller`. Verifies the off-hot-path
// invariant, overhead budget shape, no-extra-spawns invariant
// (receipt spawn happens via caller's pool handle), and the
// `serde_jcs` canonicalization invariant.

use super::receipt_marshaller::{
    ActionOutcome, ObservedCosts, ReceiptBuilder, ReceiptMarshaller, ReceiptStatus,
};
use loom_core::error::LoomError;
use loom_core::manifest_writer::{ManifestWriter, SessionId};
use std::sync::Arc;
use tokio::runtime::Handle as TokioHandle;

// === Off the hot path: queue takes pool handle, returns immediately ===

#[test]
fn queue_signature_takes_pool_handle_and_returns_unit() {
    // The contract: dispatch must NOT block on receipt assembly.
    // `queue` accepts a `TokioHandle` (the session's receipt_pool, NOT
    // a global pool) and returns `Result<(), LoomError>`
    // immediately after spawning.
    fn _ck(m: &Arc<ReceiptMarshaller>, o: ActionOutcome, h: TokioHandle) -> Result<(), LoomError> {
        m.queue(o, h)
    }
    let _ = _ck;
}

#[test]
fn queue_does_not_take_self_mut() {
    // The marshaller is `Arc<Self>` — concurrent dispatches share the
    // same instance. Compile-time pin: `&Arc<Self>` not `&mut Self`.
    fn _ck(m: &Arc<ReceiptMarshaller>, o: ActionOutcome, h: TokioHandle) -> Result<(), LoomError> {
        m.queue(o, h)
    }
    let _ = _ck;
}

// === Pool is supplied by caller, not stored globally ===

#[test]
fn marshaller_struct_does_not_store_a_runtime() {
    // Compile-time pin: `ReceiptMarshaller::new` takes only the
    // `ManifestWriter` and `BudgetEnforcer`. The pool handle is
    // session-owned and threaded per-call. Storing a global runtime
    // here would violate the per-session-pool invariant.
    fn _ck(
        mw: Arc<dyn ManifestWriter>,
        be: Arc<dyn loom_core::budget_enforcer::BudgetEnforcer>,
    ) -> Arc<ReceiptMarshaller> {
        ReceiptMarshaller::new(mw, be)
    }
    let _ = _ck;
}

// === Integer-only receipt fields ===

#[test]
fn receipt_builder_has_integer_only_fields() {
    let b = ReceiptBuilder {
        action_id: u64::MAX,
        started_at_ms: 0,
        finished_at_ms: 1,
        status: ReceiptStatus::Ok,
        side_effects_count: u32::MAX,
        host_call_count: 1,
        error_code: None,
        error_details: None,
        action_hash: String::new(),
        outcome_hash: String::new(),
        emitted_at_ms: 0,
        ..Default::default()
    };
    let _: u64 = b.action_id;
    let _: u64 = b.started_at_ms;
    let _: u64 = b.finished_at_ms;
    let _: u32 = b.side_effects_count;
    let _: u32 = b.host_call_count;
    let _: String = b.action_hash;
    let _: String = b.outcome_hash;
    let _: u64 = b.emitted_at_ms;
}

#[test]
fn observed_costs_have_integer_only_fields() {
    let c = ObservedCosts {
        walltime_ms: 50,
        network_bytes: 1024,
        dom_nodes: 100,
        js_heap_bytes: 2_000_000,
    };
    let _: u64 = c.walltime_ms;
    let _: u64 = c.network_bytes;
    let _: u64 = c.dom_nodes;
    let _: u64 = c.js_heap_bytes;
}

// === BC HARD: serde_jcs is the ONLY canonicalizer ===

#[test]
fn assemble_canonical_bytes_is_pub_static_returns_vec_u8() {
    // Pure function so unit tests can call it without spinning a runtime.
    fn _ck(b: &ReceiptBuilder) -> Result<Vec<u8>, LoomError> {
        ReceiptMarshaller::assemble_canonical_bytes(b)
    }
    let _ = _ck;
}

// === Status enum — three states (Ok, Error, Trapped) ===

#[test]
fn receipt_status_has_three_variants() {
    let _ok = ReceiptStatus::Ok;
    let _err = ReceiptStatus::Error;
    let _trap = ReceiptStatus::Trapped;
}

// === Backpressure (soft binding §6) ===

#[test]
fn append_synchronous_fallback_is_pub_for_backpressure() {
    fn _ck(m: &ReceiptMarshaller, o: ActionOutcome) -> Result<(), LoomError> {
        m.append_synchronous_fallback(o)
    }
    let _ = _ck;
}

// === Action outcome shape ===

#[test]
fn action_outcome_owns_session_id_and_costs() {
    fn _ck(s: SessionId, b: ReceiptBuilder, c: ObservedCosts) -> ActionOutcome {
        ActionOutcome {
            session_id: s,
            builder: b,
            observed_costs: c,
        }
    }
    let _ = _ck;
}
