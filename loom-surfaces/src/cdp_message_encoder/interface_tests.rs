// Interface tests for `CdpMessageEncoder`. Verifies the typed-CDP-structs
// invariant (never raw JSON) and integer-only coordinates (no floats).

extern crate alloc;

use super::cdp_message_encoder::{
    CdpMessage, CdpMessageEncoder, DomGetBoxModel, DomGetDocument, DomQuerySelector,
    DomScrollIntoViewIfNeeded, InputDispatchKeyEvent, InputDispatchMouseEvent,
    PageAddScriptToEvaluateOnNewDocument, PageCaptureScreenshot, PageGetLayoutMetrics,
    PageNavigate, RuntimeCallFunctionOn, RuntimeEvaluate, CHROMIUM_SHIM_ID, DET_INIT_JS_NAME,
};
use alloc::string::ToString;

// === typed CDP method names match the on-the-wire CDP namespace ===

#[test]
fn method_names_match_cdp_wire_namespace() {
    assert_eq!(
        CdpMessage::PageNavigate(PageNavigate {
            url: "https://x".to_string(),
            transition_type: "typed".to_string()
        })
        .method_name(),
        "Page.navigate"
    );
    assert_eq!(
        CdpMessage::PageAddScriptToEvaluateOnNewDocument(PageAddScriptToEvaluateOnNewDocument {
            source: "".to_string(),
            run_immediately: true
        })
        .method_name(),
        "Page.addScriptToEvaluateOnNewDocument"
    );
    assert_eq!(
        CdpMessage::InputDispatchMouseEvent(InputDispatchMouseEvent {
            event_type: "mousePressed".to_string(),
            x: 0,
            y: 0,
            button: "left".to_string(),
            click_count: 1,
            delta_x: None,
            delta_y: None
        })
        .method_name(),
        "Input.dispatchMouseEvent"
    );
    assert_eq!(
        CdpMessage::InputDispatchKeyEvent(InputDispatchKeyEvent {
            event_type: "keyDown".to_string(),
            text: "a".to_string(),
            key: "a".to_string(),
            code: "KeyA".to_string(),
            windows_virtual_key_code: 65,
            native_virtual_key_code: 0
        })
        .method_name(),
        "Input.dispatchKeyEvent"
    );
    assert_eq!(
        CdpMessage::RuntimeEvaluate(RuntimeEvaluate {
            expression: "1+1".to_string(),
            return_by_value: true,
            await_promise: false,
            timeout_ms: 1000
        })
        .method_name(),
        "Runtime.evaluate"
    );
    assert_eq!(
        CdpMessage::RuntimeCallFunctionOn(RuntimeCallFunctionOn {
            object_id: "x".to_string(),
            function_declaration: "function(){}".to_string(),
            arguments_json: "[]".to_string(),
            return_by_value: true
        })
        .method_name(),
        "Runtime.callFunctionOn"
    );
    assert_eq!(
        CdpMessage::DomQuerySelector(DomQuerySelector {
            node_id: 1,
            selector: "input".to_string()
        })
        .method_name(),
        "DOM.querySelector"
    );
    assert_eq!(
        CdpMessage::DomGetDocument(DomGetDocument {
            depth: -1,
            pierce: true
        })
        .method_name(),
        "DOM.getDocument"
    );
    assert_eq!(
        CdpMessage::DomGetBoxModel(DomGetBoxModel { node_id: 7 }).method_name(),
        "DOM.getBoxModel"
    );
    assert_eq!(
        CdpMessage::DomScrollIntoViewIfNeeded(DomScrollIntoViewIfNeeded { node_id: 7 })
            .method_name(),
        "DOM.scrollIntoViewIfNeeded"
    );
    assert_eq!(
        CdpMessage::PageGetLayoutMetrics(PageGetLayoutMetrics {}).method_name(),
        "Page.getLayoutMetrics"
    );
    assert_eq!(
        CdpMessage::PageCaptureScreenshot(PageCaptureScreenshot {
            format: "png".to_string(),
            quality: None,
            capture_beyond_viewport: false
        })
        .method_name(),
        "Page.captureScreenshot"
    );
}

// === encode returns CBOR bytes (Vec<u8>), not JSON String ===

#[test]
fn encode_returns_byte_vec_not_string() {
    let m = CdpMessage::PageNavigate(PageNavigate {
        url: "https://example".to_string(),
        transition_type: "typed".to_string(),
    });
    let bytes: alloc::vec::Vec<u8> = CdpMessageEncoder::encode(&m);
    // Type-level evidence: the return is `Vec<u8>`. Behavioural
    // evidence (real CBOR header byte) is exercised in integration
    // tests when the real `serde_cbor` is wired in.
    let _ = bytes;
}

// === det_init injection identifier is stable ===

#[test]
fn det_init_js_constant_is_named() {
    assert_eq!(DET_INIT_JS_NAME, "loom_det_init.js");
}

#[test]
fn add_script_to_evaluate_on_new_document_supports_run_immediately() {
    // CDP semantics: `runImmediately = true` is essential — without it
    // the page's own scripts can capture references to `Date.now` /
    // `Math.random` before our overrides install (KILL: would break determinism).
    let m = PageAddScriptToEvaluateOnNewDocument {
        source: "Date.now=()=>0".to_string(),
        run_immediately: true,
    };
    assert!(m.run_immediately);
}

// === Constants ===

#[test]
fn chromium_shim_id_is_string_constant() {
    assert_eq!(CHROMIUM_SHIM_ID, "chromium");
}

// === integer coordinates only — no floats ===

#[test]
fn mouse_event_coordinates_are_integers() {
    let m = InputDispatchMouseEvent {
        event_type: "mousePressed".to_string(),
        x: 100,
        y: 200,
        button: "left".to_string(),
        click_count: 1,
        delta_x: None,
        delta_y: None,
    };
    let _: i64 = m.x;
    let _: i64 = m.y;
    let _: u32 = m.click_count;
}

#[test]
fn scroll_wheel_deltas_are_integer_signed() {
    let m = InputDispatchMouseEvent {
        event_type: "mouseWheel".to_string(),
        x: 0,
        y: 0,
        button: "none".to_string(),
        click_count: 0,
        delta_x: Some(-50),
        delta_y: Some(120),
    };
    assert_eq!(m.delta_y, Some(120));
}

#[test]
fn screenshot_quality_is_integer_when_set() {
    let m = PageCaptureScreenshot {
        format: "jpeg".to_string(),
        quality: Some(80),
        capture_beyond_viewport: false,
    };
    let _: Option<u32> = m.quality;
}
