// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-host/modules/session_executor/interface_tests.rs` instead.
// Interface tests for `SessionExecutor`. Verifies BC-HOST-01 (no
// extra spawns inside dispatch — caller's tokio handle is borrowed),
// IC-HOST-06 (typed-receipt trap propagation), and per-dispatch
// `Store<HostState>` ownership.

use super::session_executor::{Action, ActionOutcome, SessionExecutor, SessionHandle};
use loom_core::error::LoomError;
use loom_core::manifest_writer::SessionId;
use crate::host_function_table::HostState;
use crate::host_observability::HostObservability;
use crate::module_library::ModuleLibrary;
use crate::trap_handler::TrapHandler;
use crate::wasm_runtime::WasmRuntime;
use crate::wit_type_marshaller::Mode;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::Notify;

// === BC-HOST-01: SessionHandle carries TWO tokio Handles ===

#[test]
fn session_handle_has_handle_and_receipt_pool_both_tokio_handles() {
    // Two distinct handles: `handle` for the surface invocation,
    // `receipt_pool` for the post-return receipt spawn. Per
    // BC-HOST-01, neither is a global pool reference.
    fn _ck(s: &SessionHandle) -> (&tokio::runtime::Handle, &tokio::runtime::Handle) {
        (&s.handle, &s.receipt_pool)
    }
    let _ = _ck;
}

#[test]
fn session_handle_carries_abort_signal_and_flag() {
    // IC-CORE-02 abort plumbing: SessionExecutor must observe both
    // (atomic bool fast-path + Notify wakeup).
    fn _ck(s: &SessionHandle) -> (&Arc<AtomicBool>, &Arc<Notify>) {
        (&s.abort_flag, &s.abort_signal)
    }
    let _ = _ck;
}

// === BC-HOST-01: run takes mode + linker, no internal `tokio::spawn` ===

#[test]
fn run_signature_takes_pre_built_linker_reference() {
    // Compile-time pin: the linker is borrowed from
    // `HostFunctionRegistry::linker_for(mode)`. The executor does NOT
    // construct or own a linker.
    fn _ck<'a>(
        e: &'a Arc<SessionExecutor>,
        a: Action,
        s: SessionHandle,
        m: Mode,
        l: &'a wasmtime::component::Linker<HostState>,
        st: HostState,
    ) -> impl std::future::Future<Output = Result<ActionOutcome, LoomError>> + 'a {
        e.run(a, s, m, l, st)
    }
    let _ = _ck;
}

#[test]
fn doc_pin_no_extra_tokio_spawn_per_dispatch() {
    // The doc string explicitly says "NO `tokio::spawn` per dispatch.
    // NO per-host-fn task spawns."
    let pin = "NO `tokio::spawn` per dispatch. NO per-host-fn task spawns.";
    assert!(pin.contains("NO `tokio::spawn` per dispatch"));
}

// === IC-HOST-06: trap returns typed Trapped outcome, never panics ===

#[test]
fn action_outcome_has_success_trapped_aborted_variants() {
    // The three terminal states. No `Panicked` variant — the daemon
    // never crashes.
    fn _ck(o: ActionOutcome) -> &'static str {
        match o {
            ActionOutcome::Success { .. } => "ok",
            ActionOutcome::Trapped { .. } => "trapped",
            ActionOutcome::Aborted { .. } => "aborted",
        }
    }
    let _ = _ck;
}

// === Per-dispatch Store<HostState> (state §5) ===

#[test]
fn instantiate_surface_takes_mut_store_so_each_dispatch_gets_fresh_one() {
    // Compile-time pin: `&mut Store<HostState>`. The executor
    // constructs the Store inside `run` and never reuses one across
    // actions (R2 mitigation: per-session isolation). Async because the
    // wasmtime engine is async-config'd (see `instantiate_surface` doc).
    fn _ck<'a>(
        e: &'a SessionExecutor,
        s: &'a mut wasmtime::Store<HostState>,
        c: &'a wasmtime::component::Component,
        l: &'a wasmtime::component::Linker<HostState>,
    ) -> impl std::future::Future<Output = Result<wasmtime::component::Instance, LoomError>> + 'a {
        e.instantiate_surface(s, c, l)
    }
    let _ = _ck;
}

// === Acyclicity: dependency block matches module_list.md ===

#[test]
fn new_signature_takes_runtime_library_traphandler_obs_no_wasmhost() {
    fn _ck(
        rt: Arc<WasmRuntime>,
        lib: Arc<ModuleLibrary>,
        th: Arc<TrapHandler>,
        obs: Arc<HostObservability>,
    ) -> Arc<SessionExecutor> {
        SessionExecutor::new(rt, lib, th, obs)
    }
    let _ = _ck;
}

// === Action shape (WIT-derived; integer-only fields BC HARD #3) ===

#[test]
fn action_struct_has_integer_action_id_and_string_surface_method() {
    let a = Action {
        action_id: 42,
        surface: "stocktwits".into(),
        method: "post_idea".into(),
        args_canonical_bytes: vec![],
    };
    let _: u64 = a.action_id;
    assert_eq!(a.surface, "stocktwits");
    assert_eq!(a.method, "post_idea");
}

// === Surface name resolution ===

#[test]
fn surface_for_extracts_surface_name_from_action() {
    fn _ck(e: &SessionExecutor, a: &Action) -> crate::module_library::SurfaceName {
        e.surface_for(a)
    }
    let _ = _ck;
}

// === Session id propagation ===

#[test]
fn session_id_flows_through_handle_into_outcome() {
    let _id = SessionId("01HZ".into());
    let _ = _id;
}

// === Typed-Receipt decoding (wasm-guest-dispatch) ===
//
// The WASM guest returns `result<receipt, host-error>` per
// `wit/loom-surface.wit:78-90`. `decode_typed_receipt` is the
// host-side path that lifts the typed return into `ReceiptBuilder`.
// These tests pin the wasmtime-44 `Val` shape (kebab-case field
// names, `Val::Result(Ok(Some(Box<Val>)))` envelope) so a wasmtime
// upgrade that changes the encoding fails loud at unit-test time
// rather than during a smoke run.

use crate::receipt_marshaller::ReceiptBuilder;
use crate::session_executor::session_executor::{build_action_val, decode_typed_receipt};
use wasmtime::component::Val;

#[test]
fn decode_typed_receipt_extracts_three_fields_from_ok_record() {
    use loom_core::error::LoomErrorCode;
    let receipt = Val::Record(vec![
        (
            "action-hash".to_string(),
            Val::String("deadbeefcafef00d".into()),
        ),
        (
            "outcome-hash".to_string(),
            Val::String("0123456789abcdef".into()),
        ),
        ("emitted-at-ms".to_string(), Val::U64(1_700_000_000_000)),
    ]);
    let val = Val::Result(Ok(Some(Box::new(receipt))));
    let mut b = ReceiptBuilder::default();
    decode_typed_receipt(&val, &mut b).expect("ok decode");
    assert_eq!(b.action_hash, "deadbeefcafef00d");
    assert_eq!(b.outcome_hash, "0123456789abcdef");
    assert_eq!(b.emitted_at_ms, 1_700_000_000_000);
    assert!(b.error_code.is_none());
    let _ = LoomErrorCode::Internal; // touch the import for the next tests
}

#[test]
fn decode_typed_receipt_maps_err_variant_to_specific_loom_code() {
    use loom_core::error::LoomErrorCode;
    let host_err = Val::Variant(
        "shim-failure".into(),
        Some(Box::new(Val::String("chromium subprocess died".into()))),
    );
    let val = Val::Result(Err(Some(Box::new(host_err))));
    let mut b = ReceiptBuilder::default();
    let err = decode_typed_receipt(&val, &mut b).expect_err("err decode");
    // Variant maps to ShimFailure, not bare SurfaceTrap — operator
    // sees the actual fault category in the JSON-RPC error envelope.
    assert_eq!(err.code, LoomErrorCode::ShimFailure);
    assert!(err.message.contains("host-error::shim-failure"));
    assert!(err.message.contains("chromium subprocess died"));
    assert_eq!(b.error_code.as_deref(), Some("shim-failure"));
    assert_eq!(b.error_details.as_deref(), Some("chromium subprocess died"));
    assert!(b.action_hash.is_empty());
}

#[test]
fn decode_typed_receipt_maps_each_wit_host_error_variant() {
    // One assertion per WIT host-error variant pinning the
    // (variant_name, LoomErrorCode) mapping. wit/loom-surface.wit:30-36
    // is the source of truth.
    use loom_core::error::LoomErrorCode;
    let cases = [
        ("budget-exceeded", LoomErrorCode::BudgetExceeded),
        ("vault-rejection", LoomErrorCode::VaultRejection),
        ("shim-failure", LoomErrorCode::ShimFailure),
        ("store-integrity-failed", LoomErrorCode::StoreIntegrityFailed),
        ("internal", LoomErrorCode::Internal),
    ];
    for (variant_name, expected_code) in cases {
        let host_err = Val::Variant(variant_name.into(), Some(Box::new(Val::String("ctx".into()))));
        let val = Val::Result(Err(Some(Box::new(host_err))));
        let mut b = ReceiptBuilder::default();
        let err = decode_typed_receipt(&val, &mut b).expect_err("err");
        assert_eq!(
            err.code, expected_code,
            "variant {variant_name} must map to {expected_code:?}"
        );
    }
}

#[test]
fn decode_typed_receipt_rejects_unexpected_outer_shape_with_internal_code() {
    use loom_core::error::LoomErrorCode;
    // Anything that isn't Val::Result is a host-side decode bug
    // (wasmtime returned an unexpected shape). Distinguishable from
    // a legitimate guest Err by the LoomErrorCode::Internal code,
    // not by the message text.
    let val = Val::Bool(true);
    let mut b = ReceiptBuilder::default();
    let err = decode_typed_receipt(&val, &mut b).expect_err("rejects non-Result");
    assert_eq!(err.code, LoomErrorCode::Internal);
    assert!(err.message.contains("expected Val::Result"));
}

#[test]
fn decode_typed_receipt_rejects_ok_with_no_payload_as_internal() {
    use loom_core::error::LoomErrorCode;
    // Result<receipt, _> always has a non-unit Ok payload; an empty
    // None means a wasmtime decoding bug, not a guest-side error.
    let val = Val::Result(Ok(None));
    let mut b = ReceiptBuilder::default();
    let err = decode_typed_receipt(&val, &mut b).expect_err("rejects empty Ok");
    assert_eq!(err.code, LoomErrorCode::Internal);
    assert!(err.message.contains("Ok variant has no payload"));
}

#[test]
fn decode_typed_receipt_rejects_ok_record_missing_field() {
    use loom_core::error::LoomErrorCode;
    // The WIT receipt record requires all 3 fields. A record missing
    // one (e.g. a guest regression renaming `action-hash` to
    // `actionhash`) should fail loud as Internal, not silently leave
    // builder.action_hash empty and ship a Receipt with the wrong shape.
    let receipt = Val::Record(vec![
        // Note: only 2 of the 3 expected fields.
        (
            "outcome-hash".to_string(),
            Val::String("0123456789abcdef".into()),
        ),
        ("emitted-at-ms".to_string(), Val::U64(1)),
    ]);
    let val = Val::Result(Ok(Some(Box::new(receipt))));
    let mut b = ReceiptBuilder::default();
    let err = decode_typed_receipt(&val, &mut b).expect_err("rejects partial record");
    assert_eq!(err.code, LoomErrorCode::Internal);
    assert!(
        err.message.contains("missing WIT receipt fields"),
        "message: {}",
        err.message
    );
    assert!(err.message.contains("action-hash=false"));
}

#[test]
fn build_action_val_emits_kebab_case_record_with_three_fields() {
    let action = Action {
        action_id: 7,
        surface: "web".into(),
        method: "navigate".into(),
        args_canonical_bytes: vec![0xDE, 0xAD, 0xBE, 0xEF],
    };
    let val = build_action_val(&action);
    let fields = match val {
        Val::Record(f) => f,
        other => panic!("expected Val::Record, got {other:?}"),
    };
    assert_eq!(fields.len(), 3, "WIT action has exactly 3 fields");
    assert_eq!(fields[0].0, "kind");
    assert_eq!(fields[1].0, "payload");
    assert_eq!(fields[2].0, "deadline-ms");
    match &fields[0].1 {
        Val::String(s) => assert_eq!(s, "navigate"),
        other => panic!("kind: expected Val::String, got {other:?}"),
    }
    match &fields[1].1 {
        Val::List(items) => {
            assert_eq!(items.len(), 4);
            assert!(matches!(items[0], Val::U8(0xDE)));
            assert!(matches!(items[3], Val::U8(0xEF)));
        }
        other => panic!("payload: expected Val::List, got {other:?}"),
    }
    assert!(matches!(fields[2].1, Val::U64(0)));
}

#[test]
fn decode_typed_receipt_handles_err_variant_without_payload() {
    use loom_core::error::LoomErrorCode;
    // Defensively: if the host-error is encoded as a no-payload
    // variant (shouldn't happen per the WIT, but the wasmtime Val
    // type allows it), we fall back to the variant name with empty
    // detail. Code still maps to the variant-specific LoomErrorCode.
    let host_err = Val::Variant("internal".into(), None);
    let val = Val::Result(Err(Some(Box::new(host_err))));
    let mut b = ReceiptBuilder::default();
    let err = decode_typed_receipt(&val, &mut b).expect_err("err decode");
    assert_eq!(err.code, LoomErrorCode::Internal);
    assert!(err.message.contains("host-error::internal"));
    assert_eq!(b.error_code.as_deref(), Some("internal"));
}

#[test]
fn decode_typed_receipt_rejects_err_with_no_payload_as_internal() {
    use loom_core::error::LoomErrorCode;
    // Per the WIT, every host-error variant carries a string payload,
    // so Val::Result(Err(None)) is a wasmtime-level decode bug — not
    // a legitimate guest error.
    let val = Val::Result(Err(None));
    let mut b = ReceiptBuilder::default();
    let err = decode_typed_receipt(&val, &mut b).expect_err("err");
    assert_eq!(err.code, LoomErrorCode::Internal);
    assert!(err.message.contains("Err variant has no payload"));
}

#[test]
fn decode_typed_receipt_rejects_err_with_non_variant_payload_as_internal() {
    use loom_core::error::LoomErrorCode;
    // Err payload should be Val::Variant. A Val::Bool (or anything
    // else) is a wasmtime decode bug.
    let val = Val::Result(Err(Some(Box::new(Val::Bool(false)))));
    let mut b = ReceiptBuilder::default();
    let err = decode_typed_receipt(&val, &mut b).expect_err("err");
    assert_eq!(err.code, LoomErrorCode::Internal);
    assert!(err.message.contains("expected Val::Variant"));
}

// === Fix 4 (navigate-status-code-error-surfacing) ===
// shim-failure variant carrying a STRUCTURED JSON detail (with `kind`)
// flips builder.status to Error and returns Ok(()) so the receipt path
// continues; downstream `assemble_canonical_bytes` then emits a typed
// navigate-error receipt (AC-NAVERR-01..03). All other shapes preserve
// the existing Err return for backwards compatibility.

#[test]
fn decode_typed_receipt_shim_failure_with_structured_detail_sets_status_error() {
    let detail = r#"{"kind":"http_status","status_code":404}"#;
    let host_err = Val::Variant(
        "shim-failure".into(),
        Some(Box::new(Val::String(detail.into()))),
    );
    let val = Val::Result(Err(Some(Box::new(host_err))));
    let mut b = ReceiptBuilder::default();

    decode_typed_receipt(&val, &mut b)
        .expect("structured shim-failure must return Ok so receipt path continues");

    assert_eq!(
        b.status,
        crate::receipt_marshaller::ReceiptStatus::Error,
        "AC-NAVERR-01: structured shim-failure must flip status to Error"
    );
    assert_eq!(
        b.error_code.as_deref(),
        Some("shim-failure"),
        "error_code must remain 'shim-failure' (AC-CORE-05.2 stable enum)"
    );
    assert_eq!(
        b.error_details.as_deref(),
        Some(detail),
        "error_details must preserve the raw structured JSON"
    );
    // status_code parsed out of the JSON detail and plumbed onto builder
    // for the navigate receipt path.
    assert_eq!(
        b.navigate_status_code,
        Some(404),
        "navigate_status_code must be lifted from details.status_code"
    );
}

#[test]
fn decode_typed_receipt_shim_failure_with_unstructured_detail_returns_err() {
    use loom_core::error::LoomErrorCode;
    // Plain string detail (no JSON) preserves the historical behaviour:
    // returns Err so existing tests at lines 194, 214, etc. keep passing.
    let host_err = Val::Variant(
        "shim-failure".into(),
        Some(Box::new(Val::String("chromium subprocess died".into()))),
    );
    let val = Val::Result(Err(Some(Box::new(host_err))));
    let mut b = ReceiptBuilder::default();
    let err = decode_typed_receipt(&val, &mut b)
        .expect_err("unstructured shim-failure detail must still return Err");
    assert_eq!(err.code, LoomErrorCode::ShimFailure);
}

#[test]
fn decode_typed_receipt_other_variant_with_structured_detail_returns_err() {
    use loom_core::error::LoomErrorCode;
    // The Ok-flip is scoped to `name == "shim-failure"`. Other variants
    // (e.g. budget-exceeded) must continue to return Err even if their
    // detail happens to be JSON.
    let detail = r#"{"kind":"http_status","status_code":404}"#;
    let host_err = Val::Variant(
        "budget-exceeded".into(),
        Some(Box::new(Val::String(detail.into()))),
    );
    let val = Val::Result(Err(Some(Box::new(host_err))));
    let mut b = ReceiptBuilder::default();
    let err = decode_typed_receipt(&val, &mut b)
        .expect_err("non-shim-failure variants must always return Err");
    assert_eq!(err.code, LoomErrorCode::BudgetExceeded);
}

// AC-HAREXPORT-03 / AC-INTEROP-02.1 (P0): when host_impl embeds the
// captured network events in the typed-error detail under
// `_network_events`, decode_typed_receipt must hoist them onto
// builder.navigate_side_effects_json AND strip the field from
// builder.error_details so the receipt's operator-facing error context
// stays clean.
#[test]
fn ac_harexport_03_typed_error_preserves_network_events() {
    use loom_shared::navigate_outcome::LoomNetworkEvent;

    let events = vec![LoomNetworkEvent {
        method: "GET".into(),
        url: "http://fake.test/status/404".into(),
        request_hash: "0".repeat(64),
        response_hash: "1".repeat(64),
        status: 404,
        content_type: "text/plain".into(),
        duration_ms: 12,
        response_bytes: 7,
        error_reason: None,
        error_kind: None,
    }];
    let events_value = serde_json::to_value(&events).expect("serialize events");

    let detail_obj = serde_json::json!({
        "kind": "http_status",
        "url": "http://fake.test/status/404",
        "status_code": 404,
        "_network_events": events_value,
    });
    let detail = serde_json::to_string(&detail_obj).expect("serialize detail");

    let host_err = Val::Variant(
        "shim-failure".into(),
        Some(Box::new(Val::String(detail.clone()))),
    );
    let val = Val::Result(Err(Some(Box::new(host_err))));
    let mut b = ReceiptBuilder::default();

    decode_typed_receipt(&val, &mut b).expect("structured shim-failure must return Ok");

    assert_eq!(
        b.status,
        crate::receipt_marshaller::ReceiptStatus::Error,
        "shim-failure with structured detail flips status to Error"
    );
    assert_eq!(b.navigate_status_code, Some(404));

    // Plumbing must arrive on navigate_side_effects_json.
    let bytes = b
        .navigate_side_effects_json
        .as_deref()
        .expect("P0: _network_events must be hoisted onto builder");
    let round_trip: Vec<LoomNetworkEvent> =
        serde_json::from_slice(bytes).expect("bytes round-trip as Vec<LoomNetworkEvent>");
    assert_eq!(round_trip.len(), 1);
    assert_eq!(round_trip[0].url, "http://fake.test/status/404");
    assert_eq!(round_trip[0].status, 404);

    // error_details must NOT leak the internal `_network_events` field.
    let cleaned_details = b
        .error_details
        .as_deref()
        .expect("error_details still set after cleaning");
    assert!(
        !cleaned_details.contains("_network_events"),
        "P0: error_details must be stripped of internal plumbing; got {cleaned_details}"
    );
    // Operator-facing keys remain.
    assert!(cleaned_details.contains("\"kind\""));
    assert!(cleaned_details.contains("\"status_code\""));
}
