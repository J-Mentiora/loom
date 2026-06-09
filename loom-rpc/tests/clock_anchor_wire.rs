//! `clock_anchor` wire-contract (Cluster A): the RPC params struct carries the
//! operator's `--clock-anchor`, and pre-feature clients that omit it deserialize
//! to `None` (back-compat) so default behavior is unchanged.

use loom_rpc::core_service_adapter::core_service_adapter::CreateSessionParams;

#[test]
fn clock_anchor_deserializes_from_wire() {
    let p: CreateSessionParams = serde_json::from_value(serde_json::json!({
        "profile": "standard",
        "network_mode": "live",
        "clock_anchor": 1_700_000_000_000u64,
    }))
    .expect("params with clock_anchor must deserialize");
    assert_eq!(p.clock_anchor, Some(1_700_000_000_000));
}

#[test]
fn omitted_clock_anchor_defaults_to_none() {
    let p: CreateSessionParams = serde_json::from_value(serde_json::json!({
        "profile": "standard",
        "network_mode": "live",
    }))
    .expect("pre-feature wire (no clock_anchor) must still deserialize");
    assert_eq!(p.clock_anchor, None, "back-compat: absent field → None");
}
