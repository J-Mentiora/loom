//! Integration tests — `loom-surfaces` contract (AC-WEB-07 / IC-SURF-06 / SR-SURF-03).
//!
//! Contract: `projects/loom/architecture/contracts/loom-surfaces_contract.md`
//!
//! Tests exercise native-side surface types (SafetyPolicy, CdpMessageEncoder)
//! that compile for both wasm32 and native targets. No WASM runtime required.

use loom_surfaces::cdp_message_encoder::{
    CdpMessage, CdpMessageEncoder, PageNavigate, RuntimeEvaluate, CHROMIUM_SHIM_ID,
    DET_INIT_JS_NAME,
};
use loom_surfaces::safety::{PolicyViolation, SafetyPolicy, SafetyProfile, EVALUATE_DENYLIST};

// ─── Test 1: Happy — SafetyPolicy allows benign expression under Safe profile ─

/// Contract: SafetyPolicy::check_evaluate returns None when the expression does
/// not match any EVALUATE_DENYLIST pattern (AC-WEB-07.1).
#[test]
fn test_safe_profile_allows_benign_expression() {
    let result = SafetyPolicy::check_evaluate(
        SafetyProfile::Safe,
        "document.getElementById('submit').click()",
    );
    assert_eq!(
        result, None,
        "benign expression must pass SafetyProfile::Safe; got {:?}",
        result
    );
}

// ─── Test 2: Error — SafetyPolicy blocks denylist expressions under Safe ──────

/// Contract: SafetyPolicy::check_evaluate returns EvaluateDenylistMatch when
/// the expression contains a EVALUATE_DENYLIST pattern with SafetyProfile::Safe
/// (AC-WEB-07.1). Covers all patterns in the denylist.
#[test]
fn test_safe_profile_blocks_denylist_patterns() {
    for pattern in EVALUATE_DENYLIST {
        let expression = format!("const x = {}; x.getItem('key')", pattern);
        let result = SafetyPolicy::check_evaluate(SafetyProfile::Safe, &expression);
        assert_eq!(
            result,
            Some(PolicyViolation::EvaluateDenylistMatch),
            "denylist pattern {:?} must be blocked under SafetyProfile::Safe",
            pattern
        );
    }
}

// ─── Test 3: Happy — Default profile never blocks any expression ──────────────

/// Contract: SafetyPolicy::check_evaluate returns None for any expression under
/// SafetyProfile::Default — the default profile has no evaluate restrictions
/// (AC-WEB-07.1).
#[test]
fn test_default_profile_allows_denylist_expressions() {
    for pattern in EVALUATE_DENYLIST {
        let expression = format!("{}.foo", pattern);
        let result = SafetyPolicy::check_evaluate(SafetyProfile::Default, &expression);
        assert_eq!(
            result, None,
            "SafetyProfile::Default must allow any expression; {:?} was blocked",
            pattern
        );
    }
}

// ─── Test 4: Happy — CdpMessageEncoder produces non-empty CBOR bytes ─────────

/// Contract: CdpMessageEncoder::encode produces valid CBOR bytes with the
/// correct method name (IC-SURF-06). The shim ID is "chromium" (constant).
#[test]
fn test_cdp_message_encoder_page_navigate_produces_cbor() {
    let msg = CdpMessage::PageNavigate(PageNavigate {
        url: "https://example.com".into(),
        transition_type: "typed".into(),
    });

    assert_eq!(
        msg.method_name(),
        "Page.navigate",
        "method_name must be stable wire identifier"
    );

    let bytes = CdpMessageEncoder::encode(&msg);
    assert!(!bytes.is_empty(), "encoded CBOR must not be empty");

    // CBOR map starts with 0xa2 (2-field map) – spot check the header byte.
    // This validates the wire shape is CBOR, not JSON or raw bytes.
    assert!(
        bytes[0] & 0xe0 == 0xa0,
        "CBOR encoding must start with a map major type (0xa*); got 0x{:02x}",
        bytes[0]
    );
}

// ─── Test 5: Happy — RuntimeEvaluate encodes correctly ────────────────────────

/// Contract: CdpMessageEncoder::encode produces distinct, non-empty CBOR for
/// each CDP message variant (IC-SURF-06 stable wire shape).
#[test]
fn test_cdp_message_encoder_runtime_evaluate_produces_cbor() {
    let msg = CdpMessage::RuntimeEvaluate(RuntimeEvaluate {
        expression: "document.title".into(),
        return_by_value: true,
        await_promise: false,
        timeout_ms: 5000,
    });

    assert_eq!(msg.method_name(), "Runtime.evaluate");

    let bytes = CdpMessageEncoder::encode(&msg);
    assert!(
        !bytes.is_empty(),
        "RuntimeEvaluate must encode to non-empty CBOR"
    );

    // Encoding for two different messages must differ
    let nav_bytes = CdpMessageEncoder::encode(&CdpMessage::PageNavigate(PageNavigate {
        url: "https://example.com".into(),
        transition_type: "typed".into(),
    }));
    assert_ne!(
        bytes, nav_bytes,
        "different CDP messages must produce different CBOR"
    );
}

// ─── Test 6: Happy — CHROMIUM_SHIM_ID constant is "chromium" ─────────────────

/// Contract: CHROMIUM_SHIM_ID is the stable "chromium" shim identifier used in
/// host::shim_call (IC-SURF-06). If this changes, the shim peer breaks.
#[test]
fn test_chromium_shim_id_is_stable() {
    assert_eq!(
        CHROMIUM_SHIM_ID, "chromium",
        "CHROMIUM_SHIM_ID wire constant must be 'chromium'"
    );
    assert_eq!(
        DET_INIT_JS_NAME, "loom_det_init.js",
        "DET_INIT_JS_NAME must be stable (SR-SURF-03)"
    );
}

// ─── Test 7: Happy — is_session_scoped_path path scoping ─────────────────────

/// Contract: SafetyPolicy::is_session_scoped_path returns true only for paths
/// under the session downloads dir (AC-WEB-07.2).
#[test]
fn test_is_session_scoped_path_positive_and_negative() {
    let base = "/tmp/loom/sessions/s1/downloads";

    // Positive: directly under downloads
    assert!(
        SafetyPolicy::is_session_scoped_path("/tmp/loom/sessions/s1/downloads/file.pdf", base),
        "path under session_downloads_dir must be scoped"
    );

    // Negative: sibling directory
    assert!(
        !SafetyPolicy::is_session_scoped_path("/tmp/loom/sessions/s1/manifests/data.json", base),
        "sibling dir path must NOT be scoped"
    );

    // Negative: parent directory
    assert!(
        !SafetyPolicy::is_session_scoped_path("/tmp/loom/sessions/s1", base),
        "parent dir must NOT be scoped"
    );

    // Boundary: trailing slash on base
    assert!(
        SafetyPolicy::is_session_scoped_path(
            "/tmp/loom/sessions/s1/downloads/img.png",
            "/tmp/loom/sessions/s1/downloads/",
        ),
        "trailing-slash base must still match child paths"
    );
}
