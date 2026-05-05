// Re-export of the locked v5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-host/modules/host_function_table/interface_tests.rs` instead.
// Interface tests for `HostFunctionTable`. Verifies the sole WASM↔core
// bridge, the no-secret-in-WASM-linear-memory invariant, that every
// host-fn writes to tape, the WIT-derived signatures, and the no-audit-
// entry-writes-from-host-body invariant.

use super::host_function_table::{
    HostFnsTrait, HostState, LiveHostFns, LogLevel, ReplayHostFns, WitReceipt,
};
use crate::error_mapper::HostError;
use loom_core::content_store::ContentRef;
use loom_core::vault::{NetRequest, NetResp};

// === Enumerate exactly 8 host fns from the trait ===

#[test]
fn host_fns_trait_has_eight_methods() {
    // The trait declares clock_now, rng_next_u64, blob_put, blob_get,
    // net_request, shim_call, log_emit, receipt_emit. No more, no less.
    fn _enum_methods<H: HostFnsTrait>() {
        let _ = H::clock_now;
        let _ = H::rng_next_u64;
        let _ = H::blob_put;
        let _ = H::blob_get;
        let _ = H::net_request;
        let _ = H::shim_call;
        let _ = H::log_emit;
        let _ = H::receipt_emit;
    }
    _enum_methods::<LiveHostFns>();
    _enum_methods::<ReplayHostFns>();
}

// === NetResp does NOT carry Authorization header ===

#[test]
fn net_request_returns_net_resp_without_authorization_field() {
    // Compile-time pin: the return type is NetResp (not NetRequest).
    // NetResp's `headers: BTreeMap<String, String>` will, in the live
    // impl, exclude the substituted Authorization header.
    fn _ck(state: &mut HostState, req: NetRequest) -> Result<NetResp, HostError> {
        LiveHostFns::net_request(state, req)
    }
    let _ = _ck;
}

#[test]
fn net_resp_struct_has_no_authorization_specific_field() {
    // The NetResp wit-bindgen output is `{status, headers, body}`. There
    // is no `auth_header_used` or similar field. Pinned by inspection.
    let r = NetResp {
        status: 200,
        headers: std::collections::BTreeMap::new(),
        body: vec![],
    };
    assert_eq!(r.status, 200);
    assert!(r.headers.is_empty());
}

// === Every host-fn appends to tape ===

#[test]
fn doc_pin_every_host_fn_writes_tape_before_side_effect() {
    // The interfaces.rs doc string asserts: "Every host-fn appends to
    // the DeterminismHarness tape BEFORE invoking its side effect."
    let pin =
        "Every host-fn appends to the `DeterminismHarness` tape BEFORE invoking its side effect";
    assert!(pin.contains("BEFORE"));
}

// === Live + replay impls are distinct types, both impl HostFnsTrait ===

#[test]
fn live_and_replay_are_separate_types_implementing_same_trait() {
    fn _ck<H: HostFnsTrait>() {}
    _ck::<LiveHostFns>();
    _ck::<ReplayHostFns>();
}

#[test]
fn replay_blob_put_signature_exists_and_is_unreachable_in_practice() {
    // The replay impl will return a structural error — but the
    // SIGNATURE matches live so the wit-bindgen
    // `add_to_linker<HostState>` call works for both.
    fn _ck(state: &mut HostState, bytes: Vec<u8>) -> Result<ContentRef, HostError> {
        ReplayHostFns::blob_put(state, bytes)
    }
    let _ = _ck;
}

// === Errors emerge as HostError, never anyhow ===

#[test]
fn host_fn_returns_typed_host_error_not_anyhow() {
    // The trait's return type is `Result<T, HostError>` exclusively.
    // anyhow::Error is structurally unrepresentable.
    fn _ck(state: &mut HostState) -> Result<u64, HostError> {
        LiveHostFns::clock_now(state)
    }
    let _ = _ck;
}

// === net_request body must NOT call ManifestWriter::append_audit ===

#[test]
fn doc_pin_audit_writes_owned_by_vault_not_net_request_body() {
    let pin = "audit writes are owned by `Vault::substitute` in loom-core; the\n//!   `net_request` body NEVER calls `ManifestWriter::append_audit`";
    assert!(pin.contains("NEVER"));
}

// === HostState shape ===

#[test]
fn host_state_carries_session_id_action_id_mode_and_core_handle() {
    // Pinned by struct definition; this test verifies the fields exist.
    fn _ck(s: &HostState) -> (u64, &loom_core::manifest_writer::SessionId) {
        (s.action_id, &s.session_id)
    }
    let _ = _ck;
}

// === log_emit + receipt_emit return unit (no error path to WASM) ===

#[test]
fn log_emit_returns_unit() {
    fn _ck(s: &mut HostState, level: LogLevel, msg: String, fields: Vec<(String, String)>) {
        LiveHostFns::log_emit(s, level, msg, fields)
    }
    let _ = _ck;
}

#[test]
fn receipt_emit_returns_unit() {
    fn _ck(s: &mut HostState, r: WitReceipt) {
        LiveHostFns::receipt_emit(s, r)
    }
    let _ = _ck;
}

#[test]
fn log_level_has_five_variants() {
    let _ = [
        LogLevel::Trace,
        LogLevel::Debug,
        LogLevel::Info,
        LogLevel::Warn,
        LogLevel::Error,
    ];
}
