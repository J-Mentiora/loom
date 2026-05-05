// Interface tests for `ClickVerb`. Verifies the hash-only tier,
// receipt-emit path, chromium shim usage, integer coordinates.

extern crate alloc;

use super::click_verb::{ClickAction, ClickVerb};
use alloc::string::ToString;

#[test]
fn click_action_carries_selector_button_count_timeout() {
    let a = ClickAction {
        action_id: "act_1".to_string(),
        selector: ".submit".to_string(),
        button: "left".to_string(),
        click_count: 1,
        timeout_ticks: 5_000,
    };
    assert_eq!(a.selector, ".submit");
    let _: u32 = a.click_count;
    let _: u64 = a.timeout_ticks;
}

#[test]
fn click_execute_returns_result_receipt_host_error() {
    // Type-level: WIT signature `func(action) -> result<receipt, host-error>`.
    let _: fn(ClickAction) -> Result<_, _> = ClickVerb::execute;
}

#[test]
fn click_action_takes_no_capture_mode_argument() {
    // Surface emits hash-only by default; SessionExecutor
    // upgrades to full under `--capture full`. The verb's action struct
    // has no `capture_mode` field.
    use core::mem::size_of;
    // Sanity: action is bounded-size struct (no implicit mode field).
    let _ = size_of::<ClickAction>();
}

// === Execution test (mock host) ===

#[test]
fn click_execute_returns_hash_only_receipt() {
    use crate::host_bindings::host_bindings::mock_host;
    // Default shim response (used for non-hit-test calls like the
    // post-action DOM.getDocument hash and Page.captureScreenshot).
    mock_host::setup(vec![0u8; 32]);
    // hit_test::resolve_centre_for_selector requires the 4 DOM.* + Page.*
    // responses. Register a 100×40 box at (300,200)–(400,240) so the
    // computed centre is (350, 220).
    mock_host::install_hit_test_box(300.0, 200.0, 400.0, 240.0, 1024, 768);

    let action = ClickAction {
        action_id: "act_click".to_string(),
        selector: ".submit".to_string(),
        button: "left".to_string(),
        click_count: 1,
        timeout_ticks: 5_000,
    };

    let receipt = ClickVerb::execute(action).expect("click must return Ok");

    // Hash-only tier: dom_after_ref with 64-char SHA-256
    let dom_ref = receipt.dom_after_ref.expect("dom_after_ref must be Some");
    assert_eq!(dom_ref.sha256_hex.len(), 64);
    assert!(dom_ref.sha256_hex.chars().all(|c| c.is_ascii_hexdigit()));

    // Regression guard against the original (0,0) bug: assert that
    // BOTH mouse events were dispatched at the box centre, not at page
    // origin. Box (300,200)–(400,240) → centre (350, 220).
    let dispatches = mock_host::mouse_dispatches();
    assert_eq!(
        dispatches.len(),
        2,
        "click must dispatch exactly 2 mouse events (mousePressed + mouseReleased)"
    );
    assert_eq!(dispatches[0].event_type, "mousePressed");
    assert_eq!((dispatches[0].x, dispatches[0].y), (350, 220));
    assert_eq!(dispatches[0].button, "left");
    assert_eq!(dispatches[0].click_count, 1);
    assert_eq!(dispatches[1].event_type, "mouseReleased");
    assert_eq!((dispatches[1].x, dispatches[1].y), (350, 220));
    assert_eq!(dispatches[1].button, "left");
}
