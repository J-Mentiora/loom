// Wire-shape integration test for `--clock-anchor` (cross-run determinism v2).
//
// Confirms the new `clock_anchor` field plumbs through the loom-rpc layer's
// `CreateSessionParams`:
//   1. flag present → JSON-RPC `clock_anchor: <epoch_ms>` → decodes to Some(M)
//   2. flag absent  → field omitted → defaults to None (wall-clock epoch, unchanged behavior)
//
// Stops at the loom-rpc decode boundary; deeper plumbing
// (CreateSessionParams → create_session_raw → SessionCreateOpts.started_at_ms_override
// → epoch_ms / Header started_at_ms) is exercised by the workspace compile +
// loom-core seed_threading_e2e + the real-chromium e2e Section 21.
//
// TDD: RED until `CreateSessionParams.clock_anchor` exists (Cluster A, plan step 2).

use loom_rpc::core_service_adapter::core_service_adapter::CreateSessionParams;

#[test]
fn clock_anchor_present_round_trips_via_create_session_params() {
    let json = serde_json::json!({
        "profile": "standard",
        "network_mode": "live",
        "clock_anchor": 1_700_000_000_000_u64,
    });
    let params: CreateSessionParams = serde_json::from_value(json).expect("decode succeeds");
    assert_eq!(
        params.clock_anchor,
        Some(1_700_000_000_000),
        "explicit clock_anchor must round-trip through CreateSessionParams"
    );
}

#[test]
fn clock_anchor_absent_defaults_to_none() {
    // Pre-feature CLI clients omit `clock_anchor` entirely. The serde default
    // MUST be None so the epoch falls back to wall-clock now_ms() — i.e. the
    // pre-feature behavior is byte-for-byte unchanged when the flag is unused.
    let json = serde_json::json!({
        "profile": "standard",
        "network_mode": "live",
    });
    let params: CreateSessionParams =
        serde_json::from_value(json).expect("decode succeeds even without clock_anchor field");
    assert_eq!(
        params.clock_anchor, None,
        "missing clock_anchor must default to None (wall-clock epoch, unchanged behavior)"
    );
}
