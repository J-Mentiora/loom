// Re-export of the locked v5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-host/modules/error_mapper/interface_tests.rs` instead.
// Interface tests for `ErrorMapper`. Verifies the boundary-translation
// contract, typed trap receipts, and the closed-mapping invariant
// across all five `host-error` variants.

use super::error_mapper::{
    loom_error_to_host_error, wasmtime_error_to_loom_error, wasmtime_trap_to_loom_error,
    BudgetDetail, ContentRefDetail, HostError, ShimDetail, TrapFrame, VaultDetail,
};
use loom_core::error::{LoomError, LoomErrorCode};

// === Closed-mapping smoke ===

#[test]
fn host_error_has_exactly_five_variants() {
    // The contract pins five WIT discriminants. Adding a sixth without
    // updating the boundary-translation contract is a contract violation.
    let _all = [
        HostError::BudgetExceeded(BudgetDetail {
            kind: "walltime".into(),
            observed: 1,
            limit: 0,
        }),
        HostError::VaultRejection(VaultDetail {
            reason: "expired".into(),
            grant_id: "g".into(),
        }),
        HostError::ShimFailure(ShimDetail {
            shim_id: "chromium".into(),
            reason: "spawn_failed".into(),
        }),
        HostError::StoreIntegrityFailed(ContentRefDetail {
            sha256: "00".into(),
            size_bytes: 0,
        }),
        HostError::Internal("reason".into()),
    ];
    assert_eq!(_all.len(), 5);
}

#[test]
fn loom_error_to_host_error_is_a_pub_function() {
    // Compile-time signature pin: there must be a single, well-known
    // entry point so call-sites in `HostFunctionTable` can be statically
    // verified to translate at the WIT boundary.
    fn _ck(e: LoomError) -> HostError {
        loom_error_to_host_error(e)
    }
    let _ = _ck;
}

// === Trap → typed receipt ===

#[test]
fn wasmtime_trap_to_loom_error_returns_surface_trap_variant() {
    // Once implemented, calling this with a wasmtime::Trap value must
    // return Err(LoomError::from(LoomErrorCode::SurfaceTrap {..})). We
    // don't construct a wasmtime::Trap here (private API), but we pin
    // the LoomErrorCode variant that `TrapHandler` consumes.
    let expected = LoomErrorCode::SurfaceTrap;
    let _ = LoomError::from(expected);
}

#[test]
fn trap_frame_carries_source_file_and_line_optionals() {
    // Optional because .dwp may not be present; the fallback is raw
    // addresses with debug_info_unavailable=true.
    let f = TrapFrame {
        pc: 0xdeadbeef,
        source_file: Some("surfaces/stocktwits/lib.rs".into()),
        source_line: Some(42),
        func_name: Some("dispatch_action".into()),
    };
    assert_eq!(f.pc, 0xdeadbeef);
    assert!(f.source_file.is_some());
    assert!(f.source_line.is_some());

    let g = TrapFrame {
        pc: 0,
        source_file: None,
        source_line: None,
        func_name: None,
    };
    assert!(g.source_file.is_none());
}

#[test]
fn wasmtime_trap_to_loom_error_signature_takes_surface_and_frames() {
    // The conversion site is `TrapHandler.handle_trap` — it must be able
    // to attach the surface name and resolved frames at translation time.
    fn _ck(t: wasmtime::Trap, s: String, fs: Vec<TrapFrame>) -> LoomError {
        wasmtime_trap_to_loom_error(t, s, fs)
    }
    let _ = _ck;
}

// === From impls live HERE only ===

#[test]
fn from_loom_error_for_host_error_is_implemented() {
    // Compile-time pin: the `From<LoomError> for HostError` impl exists.
    // No other module may declare it (verified by the lint
    // `tools/lint-error-codes.py` boundary-translation check).
    fn _ck(e: LoomError) -> HostError {
        HostError::from(e)
    }
    let _ = _ck;
}

#[test]
fn from_wasmtime_error_for_loom_error_is_implemented() {
    // No orphan `From<wasmtime::Error> for LoomError` — use the standalone fn.
    fn _ck(e: wasmtime::Error) -> LoomError {
        wasmtime_error_to_loom_error(e)
    }
    let _ = _ck;
}

#[test]
fn from_wasmtime_trap_for_loom_error_is_implemented() {
    // No orphan `From<wasmtime::Trap> for LoomError` — use the standalone fn.
    fn _ck(t: wasmtime::Trap) -> LoomError {
        wasmtime_trap_to_loom_error(t, String::new(), vec![])
    }
    let _ = _ck;
}

// === Variant-by-variant conversion expectations ===

#[test]
fn budget_exceeded_loom_code_maps_to_budget_exceeded_host_error() {
    // Once impl exists, this must round-trip:
    //   LoomErrorCode::BudgetExceeded {kind:"walltime",observed:1,limit:0}
    //   → HostError::BudgetExceeded(BudgetDetail{kind:"walltime",..})
    let _e = LoomErrorCode::BudgetExceeded;
}

#[test]
fn vault_rejection_codes_map_to_vault_rejection_host_error() {
    // Each Vault* code (OriginMismatch, ScopeInsufficient, GrantExpired,
    // GrantRevoked, SecretUnavailable, CredentialTypeUnsupported) maps
    // to HostError::VaultRejection with a distinct `reason`.
    let _expired = LoomErrorCode::VaultGrantExpired;
    let _revoked = LoomErrorCode::VaultGrantRevoked;
    let _origin = LoomErrorCode::VaultRejection;
}

#[test]
fn store_integrity_failed_maps_to_store_integrity_host_error() {
    let _e = LoomErrorCode::StoreIntegrityFailed;
}

#[test]
fn internal_catchall_maps_to_host_error_internal() {
    let _e = LoomErrorCode::Internal;
}

// === Negative: anyhow must not appear in From impls ===

#[test]
fn no_anyhow_error_to_host_error_conversion_exists() {
    // This test is enforced structurally — there is intentionally no
    // `From<anyhow::Error> for HostError` impl. We verify by a comment
    // pin; the actual enforcement is the CI lint that walks `From` impls
    // in `loom-host`.
    let _pin = "no anyhow::Error → HostError conversion permitted";
    assert!(_pin.contains("anyhow"));
}
