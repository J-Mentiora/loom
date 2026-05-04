// Interface tests for `ScreenshotVerb`. Verifies IC-SURF-07
// screenshot-only tier + BC-SURF-05 integer quality.


extern crate alloc;

use super::screenshot_verb::{ScreenshotAction, ScreenshotVerb};
use alloc::string::ToString;

#[test]
fn screenshot_action_carries_format_quality_viewport_timeout() {
    let a = ScreenshotAction {
        action_id: "ss".to_string(),
        format: "png".to_string(),
        quality: None,
        capture_beyond_viewport: true,
        timeout_ticks: 5_000,
    };
    assert_eq!(a.format, "png");
    assert!(a.capture_beyond_viewport);
}

#[test]
fn screenshot_quality_when_jpeg_is_integer() {
    let a = ScreenshotAction {
        action_id: "ssj".to_string(),
        format: "jpeg".to_string(),
        quality: Some(85),
        capture_beyond_viewport: false,
        timeout_ticks: 5_000,
    };
    let _: Option<u32> = a.quality;
    assert_eq!(a.quality, Some(85));
}

#[test]
fn screenshot_execute_returns_result_receipt_host_error() {
    let _: fn(ScreenshotAction) -> Result<_, _> = ScreenshotVerb::execute;
}

// === Execution test (mock host) ===

#[test]
fn screenshot_execute_returns_png_ref() {
    use crate::host_bindings::host_bindings::mock_host;
    // PNG magic bytes as mock shim response
    mock_host::setup(vec![0x89u8, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);

    let action = ScreenshotAction {
        action_id: "act_ss".to_string(),
        format: "png".to_string(),
        quality: None,
        capture_beyond_viewport: false,
        timeout_ticks: 5_000,
    };

    let receipt = ScreenshotVerb::execute(action).expect("screenshot must return Ok");

    // AC-WEB-05.1: screenshot_after_ref with 64-char SHA-256
    let ss_ref = receipt.screenshot_after_ref.expect("screenshot_after_ref must be Some");
    assert_eq!(ss_ref.sha256_hex.len(), 64);
    assert!(ss_ref.sha256_hex.chars().all(|c| c.is_ascii_hexdigit()));
    // Screenshot-only tier: no DOM blob
    assert!(receipt.dom_after_ref.is_none());
}
