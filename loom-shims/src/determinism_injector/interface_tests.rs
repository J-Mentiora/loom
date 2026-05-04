// Interface tests for `DeterminismInjector`.
// Verifies IC-SHIM-05 (R3 LOAD-BEARING — runImmediately + canonical
// method name), error mapping, source-validation guard.

use super::determinism_injector::{
    build_inject_params, script_source_has_determinism_markers, DeterminismError,
    ADD_SCRIPT_METHOD, RUN_IMMEDIATELY,
};
use crate::ipc_endpoint::ipc_endpoint::ShimErrorCode;
use ciborium::value::Value;

// === IC-SHIM-05 (KILL): canonical CDP method name ===

#[test]
fn cdp_method_is_page_add_script_to_evaluate_on_new_document() {
    assert_eq!(ADD_SCRIPT_METHOD, "Page.addScriptToEvaluateOnNewDocument");
}

// === IC-SHIM-05 (KILL): runImmediately=true is load-bearing ===

#[test]
#[allow(clippy::assertions_on_constants)]
fn run_immediately_is_true() {
    assert!(RUN_IMMEDIATELY); // IC-SHIM-05 KILL: constant must stay true
}

#[test]
fn build_inject_params_sets_run_immediately_true() {
    let params = build_inject_params("var x = 1;");
    if let Value::Map(m) = params {
        let key = Value::Text("runImmediately".into());
        let v = m.iter().find(|(k, _)| k == &key).map(|(_, v)| v).expect("runImmediately key");
        assert_eq!(v, &Value::Bool(true));
    } else {
        panic!("expected Map");
    }
}

#[test]
fn build_inject_params_carries_source() {
    let src = "Date.now = function() { return 0; };";
    let params = build_inject_params(src);
    if let Value::Map(m) = params {
        let key = Value::Text("source".into());
        let v = m.iter().find(|(k, _)| k == &key).map(|(_, v)| v).expect("source key");
        assert_eq!(v, &Value::Text(src.to_string()));
    } else {
        panic!("expected Map");
    }
}

// === Source-validation guard (refuse to boot with empty/corrupted source) ===

#[test]
fn script_source_validates_canonical_markers() {
    // Template-shape (post-PR): the validator requires the three
    // substitution tokens AND the canonical determinism markers.
    let good = r#"
        var _epoch_ms = __LOOM_EPOCH_MS__;
        var _c = __LOOM_SEED_LO__ | 0;
        var _d = __LOOM_SEED_HI__ | 0;
        Date.now = function() { return _epoch_ms; };
        Math.random = function() { return 0; };
        requestAnimationFrame = function(cb) { setTimeout(cb, 16); };
    "#;
    assert!(script_source_has_determinism_markers(good));
}

#[test]
fn script_source_rejected_when_missing_markers() {
    assert!(!script_source_has_determinism_markers(""));
    assert!(!script_source_has_determinism_markers("var x = 1;"));
    assert!(!script_source_has_determinism_markers("Date.now = 0;")); // missing rng + raf
}

// === IC-SHIM-10: error mapping ===

#[test]
fn empty_source_maps_to_shim_internal_error() {
    let code: ShimErrorCode = DeterminismError::EmptySource.into();
    assert_eq!(code, ShimErrorCode::ShimInternalError);
}

#[test]
fn target_detached_maps_to_chromium_unavailable() {
    let code: ShimErrorCode = DeterminismError::TargetDetached(7).into();
    assert_eq!(code, ShimErrorCode::ChromiumUnavailable);
}

#[test]
fn cdp_failure_passes_through_via_inner_mapping() {
    use crate::cdp_connection::cdp_connection::CdpError;
    let code: ShimErrorCode = DeterminismError::CdpFailure(CdpError::Timeout { ms: 50 }).into();
    assert_eq!(code, ShimErrorCode::CdpTimeout);
}

#[test]
fn cdp_protocol_inside_determinism_maps_to_cdp_protocol_error() {
    use crate::cdp_connection::cdp_connection::CdpError;
    let code: ShimErrorCode =
        DeterminismError::CdpFailure(CdpError::Protocol("bad".into())).into();
    assert_eq!(code, ShimErrorCode::CdpProtocolError);
}
