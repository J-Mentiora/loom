// Interface tests for `NavigateVerb`. Verifies SR-SURF-03 (det_init
// injection BEFORE Page.navigate), IC-SURF-07 (full-blob tier),
// IC-SURF-09 (sole receipt_emit path), and SR-SURF-05 (no replay-mode
// branching).

extern crate alloc;

use super::navigate_verb::{NavigateAction, NavigateStep, NavigateVerb};
use alloc::string::ToString;

// === SR-SURF-03: det_init injection precedes Page.navigate ===

#[test]
fn det_init_injection_runs_before_page_navigate() {
    let steps = NavigateVerb::canonical_steps();
    let inject_pos = steps
        .iter()
        .position(|s| matches!(s, NavigateStep::InjectDetInit))
        .expect("det_init injection step must exist");
    let nav_pos = steps
        .iter()
        .position(|s| matches!(s, NavigateStep::PageNavigate))
        .expect("page-navigate step must exist");
    assert!(
        inject_pos < nav_pos,
        "SR-SURF-03 KILL: det_init must run BEFORE Page.navigate"
    );
}

// === IC-SURF-07: navigate captures DOM + screenshot blobs ===

#[test]
fn navigate_captures_dom_and_screenshot_blobs() {
    let steps = NavigateVerb::canonical_steps();
    assert!(
        steps.contains(&NavigateStep::BlobPutDom),
        "IC-SURF-07: navigate full-blob tier requires DOM blob_put"
    );
    assert!(
        steps.contains(&NavigateStep::BlobPutScreenshot),
        "IC-SURF-07: navigate full-blob tier requires screenshot blob_put"
    );
}

// === IC-SURF-02: two clock_now reads bracket the action (timing_ticks) ===

#[test]
fn navigate_brackets_action_with_two_clock_reads() {
    let steps = NavigateVerb::canonical_steps();
    let starts = steps
        .iter()
        .filter(|s| matches!(s, NavigateStep::ClockNow))
        .count();
    let ends = steps
        .iter()
        .filter(|s| matches!(s, NavigateStep::ClockNowEnd))
        .count();
    assert_eq!(starts, 1, "exactly one t_start clock_now read");
    assert_eq!(ends, 1, "exactly one t_end clock_now read");
    let s_pos = steps
        .iter()
        .position(|s| matches!(s, NavigateStep::ClockNow))
        .unwrap();
    let e_pos = steps
        .iter()
        .position(|s| matches!(s, NavigateStep::ClockNowEnd))
        .unwrap();
    assert!(s_pos < e_pos, "t_start precedes t_end");
}

// === IC-SURF-09: receipt_emit is the LAST host-fn the verb makes ===

#[test]
fn receipt_emit_is_final_step() {
    let steps = NavigateVerb::canonical_steps();
    assert_eq!(
        *steps.last().unwrap(),
        NavigateStep::ReceiptEmit,
        "IC-SURF-09: receipt_emit MUST be the last verb operation"
    );
}

#[test]
fn exactly_one_receipt_emit_per_invocation() {
    let steps = NavigateVerb::canonical_steps();
    let n = steps
        .iter()
        .filter(|s| matches!(s, NavigateStep::ReceiptEmit))
        .count();
    assert_eq!(n, 1);
}

// === Action shape ===

#[test]
fn navigate_action_carries_url_and_action_id_and_timeout() {
    let a = NavigateAction {
        action_id: "act_123".to_string(),
        url: "https://example.com".to_string(),
        referrer: None,
        timeout_ticks: 30_000,
    };
    assert_eq!(a.url, "https://example.com");
    let _: u64 = a.timeout_ticks;
}

// === SR-SURF-05: no replay-mode branch in NavigateVerb ===
//
// Compile-time evidence: `execute` takes (NavigateAction) — NO `mode`
// parameter, NO `host::current_mode()` call (that host-fn does not
// exist). The CI lint `tools/lint-surface-mode.py` greps for `Mode::Replay`
// in surface crates and fails on any match.

#[test]
fn navigate_execute_takes_no_mode_argument() {
    let _: fn(NavigateAction) -> Result<_, _> = NavigateVerb::execute;
}

// === Execution tests (mock host) ===

#[test]
fn navigate_execute_calls_host_fns_in_order() {
    use crate::host_bindings::host_bindings::mock_host::{self, HostCall};
    mock_host::setup(vec![0u8; 8]);

    let action = NavigateAction {
        action_id: "act_nav".to_string(),
        url: "https://example.com".to_string(),
        referrer: None,
        timeout_ticks: 30_000,
    };

    let result = NavigateVerb::execute(action);
    assert!(result.is_ok());

    let calls = mock_host::calls();
    assert_eq!(calls.len(), 9, "expected 9 host calls; got: {:?}", calls);
    assert!(matches!(calls[0], HostCall::ClockNow));
    assert!(matches!(&calls[1], HostCall::ShimCall { shim_id, .. } if shim_id == "chromium"));
    assert!(matches!(&calls[2], HostCall::ShimCall { shim_id, .. } if shim_id == "chromium"));
    assert!(matches!(&calls[3], HostCall::ShimCall { shim_id, .. } if shim_id == "chromium"));
    assert!(matches!(calls[4], HostCall::BlobPut { .. }));
    assert!(matches!(&calls[5], HostCall::ShimCall { shim_id, .. } if shim_id == "chromium"));
    assert!(matches!(calls[6], HostCall::BlobPut { .. }));
    assert!(matches!(calls[7], HostCall::ClockNow));
    assert!(matches!(calls[8], HostCall::ReceiptEmit));
}

#[test]
fn navigate_execute_returns_receipt_with_dom_and_screenshot_refs() {
    use crate::host_bindings::host_bindings::mock_host;
    mock_host::setup(vec![0u8; 64]);

    let action = NavigateAction {
        action_id: "act_dom".to_string(),
        url: "https://example.com".to_string(),
        referrer: None,
        timeout_ticks: 10_000,
    };

    let receipt = NavigateVerb::execute(action).expect("navigate must return Ok");

    let dom_ref = receipt.dom_after_ref.expect("dom_after_ref must be Some");
    assert_eq!(dom_ref.sha256_hex.len(), 64);
    assert!(dom_ref.sha256_hex.chars().all(|c| c.is_ascii_hexdigit()));

    let ss_ref = receipt
        .screenshot_after_ref
        .expect("screenshot_after_ref must be Some");
    assert_eq!(ss_ref.sha256_hex.len(), 64);
    assert!(ss_ref.sha256_hex.chars().all(|c| c.is_ascii_hexdigit()));
}
