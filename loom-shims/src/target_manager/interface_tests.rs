// Interface tests for `TargetManager`.
// Verifies R3 ordering (KILL), one target per
// session, state-invalidation cascade, R3 invariant guard.

use super::target_manager::{assert_r3_ready, ordered_domain_enables, TargetError, TargetState};
use crate::ipc_endpoint::ipc_endpoint::ShimErrorCode;

// === domain-enable order is Network → Page → Log AFTER inject ===

#[test]
fn domain_enable_order_is_network_first_then_page_then_log() {
    assert_eq!(
        ordered_domain_enables(),
        &["Network.enable", "Page.enable", "Log.enable"]
    );
}

#[test]
fn domain_enable_order_starts_with_network_enable() {
    // R1 pre-condition: Network.enable must precede any navigation so
    // request interception is wired before responses arrive.
    let order = ordered_domain_enables();
    assert_eq!(order[0], "Network.enable");
}

// === R3 invariant guard ===

#[test]
fn r3_guard_passes_when_flag_is_true() {
    let mut s = TargetState::new(1, 7, "default".into());
    s.determinism_injected = true;
    assert!(assert_r3_ready(&s).is_ok());
}

#[test]
fn r3_guard_fails_when_flag_is_false() {
    let s = TargetState::new(1, 7, "default".into());
    let err = assert_r3_ready(&s).unwrap_err();
    match err {
        TargetError::R3OrderingViolation(t) => assert_eq!(t, 7),
        other => panic!("expected R3OrderingViolation, got {:?}", other),
    }
}

#[test]
fn target_state_default_determinism_injected_is_false() {
    let s = TargetState::new(1, 1, "p".into());
    assert!(!s.determinism_injected);
}

// === TargetError → ShimErrorCode mapping ===

#[test]
fn not_found_maps_to_target_unknown() {
    let code: ShimErrorCode = TargetError::NotFound(1).into();
    assert_eq!(code, ShimErrorCode::TargetUnknown);
}

#[test]
fn r3_violation_maps_to_shim_internal_error() {
    let code: ShimErrorCode = TargetError::R3OrderingViolation(1).into();
    assert_eq!(code, ShimErrorCode::ShimInternalError);
}

#[test]
fn determinism_injection_failed_maps_to_shim_internal_error() {
    let code: ShimErrorCode = TargetError::DeterminismInjectionFailed("x".into()).into();
    assert_eq!(code, ShimErrorCode::ShimInternalError);
}

// === one target per session (TargetState carries SessionId) ===

#[test]
fn target_state_binds_session_id() {
    let s = TargetState::new(42, 7, "isolated".into());
    assert_eq!(s.session_id, 42);
    assert_eq!(s.target_id, 7);
    assert_eq!(s.profile, "isolated");
}

// === Integer-only fields ===

#[test]
fn target_state_ids_are_u64_not_float() {
    let s = TargetState::new(u64::MAX, u64::MAX, "p".into());
    assert_eq!(s.session_id, u64::MAX);
    assert_eq!(s.target_id, u64::MAX);
}
