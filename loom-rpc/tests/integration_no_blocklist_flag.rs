// Wire-shape integration test for `--no-blocklist` (AC-DET-05.1, AC-BLOCKLIST-04).
//
// Confirms BOTH plumbing directions through the loom-rpc layer's
// `CreateSessionParams`:
//   1. CLI flag present  → JSON-RPC `no_blocklist: true`  → core sees `true`
//   2. CLI flag absent   → JSON-RPC field omitted          → core defaults to `false`
//
// Stops at the loom-rpc decode boundary; deeper plumbing
// (SessionCreateOpts → Session.no_blocklist → HostState → ShimRequest)
// is exercised by the workspace-wide compile + behavior tests.

use loom_rpc::core_service_adapter::core_service_adapter::CreateSessionParams;

#[test]
fn no_blocklist_true_round_trips_via_create_session_params() {
    let json = serde_json::json!({
        "profile": "safe",
        "network_mode": "live",
        "no_blocklist": true,
    });
    let params: CreateSessionParams = serde_json::from_value(json).expect("decode succeeds");
    assert!(
        params.no_blocklist,
        "explicit no_blocklist=true must round-trip through CreateSessionParams"
    );
}

#[test]
fn no_blocklist_absent_defaults_to_false_via_create_session_params() {
    // Pre-feature CLI clients omit `no_blocklist` entirely. The serde
    // default for the field MUST be `false` (blocklist enforced) —
    // otherwise the safe-default-on policy is broken at the wire.
    let json = serde_json::json!({
        "profile": "safe",
        "network_mode": "live",
    });
    let params: CreateSessionParams =
        serde_json::from_value(json).expect("decode succeeds even without no_blocklist field");
    assert!(
        !params.no_blocklist,
        "missing no_blocklist field must default to false (blocklist enforced)"
    );
}

#[test]
fn no_blocklist_false_is_explicit_round_trip() {
    let json = serde_json::json!({
        "profile": "safe",
        "network_mode": "live",
        "no_blocklist": false,
    });
    let params: CreateSessionParams = serde_json::from_value(json).unwrap();
    assert!(!params.no_blocklist);
}
