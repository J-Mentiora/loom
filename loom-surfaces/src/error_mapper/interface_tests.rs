// Interface tests for `ErrorMapper`. Verifies the `host-error` →
// `LoomErrorCode` table from design.md §4 and the IC-SURF-11 boundary
// contract.


extern crate alloc;

use super::error_mapper::{
    BudgetKind, ErrorMapper, HostError, LoomErrorCode, ShimFailureKind, SurfaceContext,
    VaultRejectionReason,
};
use alloc::string::String;
use alloc::string::ToString;

// === IC-SURF-11 / design.md §4 mapping table — budget-exceeded ===

#[test]
fn budget_wall_clock_maps_to_budget_wall_clock_exceeded() {
    let r = ErrorMapper::map(
        HostError::BudgetExceeded { kind: BudgetKind::WallClock },
        SurfaceContext::Web,
    );
    assert_eq!(r, LoomErrorCode::BudgetWallClockExceeded);
}

#[test]
fn budget_network_bytes_maps_to_budget_network_bytes_exceeded() {
    let r = ErrorMapper::map(
        HostError::BudgetExceeded { kind: BudgetKind::NetworkBytes },
        SurfaceContext::Web,
    );
    assert_eq!(r, LoomErrorCode::BudgetNetworkBytesExceeded);
}

#[test]
fn budget_dom_nodes_maps_to_budget_dom_node_count_exceeded() {
    let r = ErrorMapper::map(
        HostError::BudgetExceeded { kind: BudgetKind::DomNodeCount },
        SurfaceContext::Web,
    );
    assert_eq!(r, LoomErrorCode::BudgetDomNodeCountExceeded);
}

#[test]
fn budget_js_heap_maps_to_budget_js_heap_bytes_exceeded() {
    let r = ErrorMapper::map(
        HostError::BudgetExceeded { kind: BudgetKind::JsHeapBytes },
        SurfaceContext::Web,
    );
    assert_eq!(r, LoomErrorCode::BudgetJsHeapBytesExceeded);
}

// === vault-rejection ===

#[test]
fn vault_expired_maps_to_vault_grant_expired() {
    let r = ErrorMapper::map(
        HostError::VaultRejection { reason: VaultRejectionReason::Expired },
        SurfaceContext::Web,
    );
    assert_eq!(r, LoomErrorCode::VaultGrantExpired);
}

#[test]
fn vault_scope_maps_to_vault_scope_violation() {
    let r = ErrorMapper::map(
        HostError::VaultRejection { reason: VaultRejectionReason::Scope },
        SurfaceContext::Web,
    );
    assert_eq!(r, LoomErrorCode::VaultScopeViolation);
}

#[test]
fn vault_origin_maps_to_vault_origin_violation() {
    let r = ErrorMapper::map(
        HostError::VaultRejection { reason: VaultRejectionReason::Origin },
        SurfaceContext::Web,
    );
    assert_eq!(r, LoomErrorCode::VaultOriginViolation);
}

#[test]
fn vault_revoked_maps_to_vault_grant_revoked() {
    let r = ErrorMapper::map(
        HostError::VaultRejection { reason: VaultRejectionReason::Revoked },
        SurfaceContext::Web,
    );
    assert_eq!(r, LoomErrorCode::VaultGrantRevoked);
}

// === shim-failure (web context — IC-SURF-06) ===

#[test]
fn web_shim_timeout_maps_to_web_action_timeout() {
    let r = ErrorMapper::map(
        HostError::ShimFailure { kind: ShimFailureKind::Timeout },
        SurfaceContext::Web,
    );
    assert_eq!(r, LoomErrorCode::WebActionTimeout);
}

#[test]
fn non_web_shim_timeout_maps_to_surface_shim_failed() {
    let r = ErrorMapper::map(
        HostError::ShimFailure { kind: ShimFailureKind::Timeout },
        SurfaceContext::Shell,
    );
    assert_eq!(r, LoomErrorCode::SurfaceShimFailed);
}

#[test]
fn shim_selector_not_found_maps_to_web_selector_not_found() {
    let r = ErrorMapper::map(
        HostError::ShimFailure { kind: ShimFailureKind::SelectorNotFound },
        SurfaceContext::Web,
    );
    assert_eq!(r, LoomErrorCode::WebSelectorNotFound);
}

#[test]
fn shim_permission_denied_carries_permission_name() {
    let r = ErrorMapper::map(
        HostError::ShimFailure {
            kind: ShimFailureKind::PermissionDenied {
                permission: "ScreenCaptureKit".to_string(),
            },
        },
        SurfaceContext::Web,
    );
    match r {
        LoomErrorCode::TccPermissionDenied { permission } => {
            assert_eq!(permission, "ScreenCaptureKit");
        }
        other => panic!("expected TccPermissionDenied, got {:?}", other),
    }
}

#[test]
fn shim_crashed_maps_to_surface_shim_failed() {
    let r = ErrorMapper::map(
        HostError::ShimFailure { kind: ShimFailureKind::Crashed },
        SurfaceContext::Web,
    );
    assert_eq!(r, LoomErrorCode::SurfaceShimFailed);
}

#[test]
fn shim_navigation_failed_maps_to_web_navigation_failed() {
    let r = ErrorMapper::map(
        HostError::ShimFailure { kind: ShimFailureKind::NavigationFailed },
        SurfaceContext::Web,
    );
    assert_eq!(r, LoomErrorCode::WebNavigationFailed);
}

// AC-CLICK-03 / AC-HOVER-03 / AC-SCROLL-03: hit-test failure surfaces as
// `WebHitTestFailed`. Constructed inside the WASM guest by the `hit_test`
// helper when an element gives no usable box-model geometry.
#[test]
fn shim_hit_test_failed_maps_to_web_hit_test_failed() {
    let r = ErrorMapper::map(
        HostError::ShimFailure { kind: ShimFailureKind::HitTestFailed },
        SurfaceContext::Web,
    );
    assert_eq!(r, LoomErrorCode::WebHitTestFailed);
}

#[test]
fn shim_hit_test_failed_maps_to_web_hit_test_failed_in_non_web_context_too() {
    // The mapper folds context only for Timeout. HitTestFailed is web-specific
    // by construction (only Click/Hover/Scroll in the web surface emit it).
    let r = ErrorMapper::map(
        HostError::ShimFailure { kind: ShimFailureKind::HitTestFailed },
        SurfaceContext::Shell,
    );
    assert_eq!(r, LoomErrorCode::WebHitTestFailed);
}

// === store + internal ===

#[test]
fn store_integrity_failed_passthrough() {
    let r = ErrorMapper::map(HostError::StoreIntegrityFailed, SurfaceContext::Web);
    assert_eq!(r, LoomErrorCode::StoreIntegrityFailed);
}

#[test]
fn internal_carries_reason() {
    let r = ErrorMapper::map(
        HostError::Internal { reason: "wasm trap".to_string() },
        SurfaceContext::Web,
    );
    match r {
        LoomErrorCode::HostInternalError { reason } => assert_eq!(reason, "wasm trap"),
        other => panic!("expected HostInternalError, got {:?}", other),
    }
}

// === ErrorMapper has no in-WASM panic-catch path (IC-SURF-11) ===
// `std::panic::catch_unwind` is denied by `cargo-deny` at the crate
// level. This file deliberately omits any unwind-catch usage; the
// crate-level deny.toml is the structural enforcement.
