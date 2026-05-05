// Interface tests for `SnapshotVerb`. Verifies the full-blob tier and
// DOM blob hashing via blob_put.

extern crate alloc;

use super::snapshot_verb::{SnapshotAction, SnapshotVerb};
use alloc::string::ToString;

#[test]
fn snapshot_action_carries_screenshot_flag_and_timeout() {
    let a = SnapshotAction {
        action_id: "snap".to_string(),
        include_screenshot: true,
        timeout_ticks: 10_000,
    };
    assert!(a.include_screenshot);
    let _: u64 = a.timeout_ticks;
}

#[test]
fn snapshot_dom_only_variant() {
    let a = SnapshotAction {
        action_id: "snap-dom".to_string(),
        include_screenshot: false,
        timeout_ticks: 10_000,
    };
    assert!(!a.include_screenshot);
}

#[test]
fn snapshot_execute_returns_result_receipt_host_error() {
    let _: fn(SnapshotAction) -> Result<_, _> = SnapshotVerb::execute;
}

// === Execution tests (mock host) ===

#[test]
fn snapshot_execute_returns_full_blob_receipt_with_screenshot() {
    use crate::host_bindings::host_bindings::mock_host;
    mock_host::setup(vec![0xABu8, 0xCD, 0xEF]);

    let action = SnapshotAction {
        action_id: "act_snap".to_string(),
        include_screenshot: true,
        timeout_ticks: 10_000,
    };

    let receipt = SnapshotVerb::execute(action).expect("snapshot must return Ok");

    // Full-blob tier: dom_after_ref + screenshot_after_ref both populated
    let dom_ref = receipt.dom_after_ref.expect("dom_after_ref must be Some");
    assert_eq!(dom_ref.sha256_hex.len(), 64);
    assert!(dom_ref.sha256_hex.chars().all(|c| c.is_ascii_hexdigit()));

    let ss_ref = receipt
        .screenshot_after_ref
        .expect("screenshot_after_ref must be Some");
    assert_eq!(ss_ref.sha256_hex.len(), 64);
    assert!(ss_ref.sha256_hex.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn snapshot_dom_only_skips_screenshot() {
    use crate::host_bindings::host_bindings::mock_host;
    mock_host::setup(vec![0x01u8, 0x02]);

    let action = SnapshotAction {
        action_id: "act_snap_dom".to_string(),
        include_screenshot: false,
        timeout_ticks: 10_000,
    };

    let receipt = SnapshotVerb::execute(action).expect("snapshot must return Ok");

    // DOM blob must be present; screenshot must be absent
    assert!(
        receipt.dom_after_ref.is_some(),
        "dom_after_ref must be Some"
    );
    assert!(
        receipt.screenshot_after_ref.is_none(),
        "snapshot dom-only must not capture screenshot"
    );
}
