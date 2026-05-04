// CdpMessageEncoder — typed CDP-message structs serialized to CBOR for
// `host::shim_call("chromium", &cbor)`.
//
// # Contract semantics
// - **Typed structs only (IC-SURF-06).** No raw JSON; no string-templated
//   CDP. Every CDP method we use is a Rust struct with `#[derive(Serialize)]`
//   under a `serde_cbor`-compatible profile. The chromium shim is the
//   only thing that converts CBOR back to CDP JSON on the wire.
// - **Pure leaf.** No host-fn calls, no allocation outside the produced
//   `Vec<u8>`, no stateful encoder. Returns `Vec<u8>` directly; CBOR
//   encoding is infallible for these fixed-shape structs (would only
//   fail on OOM, which traps).
// - **Stable wire shape.** Field order in the structs matches the CDP
//   method's parameter order; renaming a field is a breaking change to
//   the chromium-shim peer.
// - **No `f32`/`f64`.** Coordinates are integers (CDP allows but we
//   restrict per BC-SURF-05).
//
// # Banned in this module
// - `serde_json`, `std::time`, `std::net`, raw CDP JSON strings.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use serde::Serialize;

/// Chromium shim ID used in `host::shim_call`. Constant per IC-SURF-06.
pub const CHROMIUM_SHIM_ID: &str = "chromium";

/// CDP `Page.navigate` request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PageNavigate {
    pub url: String,
    /// Always `"typed"` for human-style navigation; constant for v1.
    pub transition_type: String,
}

/// CDP `Page.addScriptToEvaluateOnNewDocument` — used for det_init.js
/// injection BEFORE `Page.navigate` (SR-SURF-03).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PageAddScriptToEvaluateOnNewDocument {
    pub source: String,
    /// `runImmediately = true` so injection wins the race against page scripts.
    pub run_immediately: bool,
}

/// CDP `Input.dispatchMouseEvent` — used for click, hover, scroll.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InputDispatchMouseEvent {
    /// One of: mousePressed, mouseReleased, mouseMoved, mouseWheel.
    pub event_type: String,
    pub x: i64,
    pub y: i64,
    /// "left" | "right" | "middle" | "none".
    pub button: String,
    pub click_count: u32,
    /// For mouseWheel: signed delta in CSS pixels.
    pub delta_x: Option<i64>,
    pub delta_y: Option<i64>,
}

/// CDP `Input.dispatchKeyEvent` — used per-char for type-text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InputDispatchKeyEvent {
    /// One of: keyDown, keyUp, char, rawKeyDown.
    pub event_type: String,
    pub text: String,
    pub key: String,
    pub code: String,
    pub windows_virtual_key_code: u32,
    pub native_virtual_key_code: u32,
}

/// CDP `Runtime.evaluate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeEvaluate {
    pub expression: String,
    pub return_by_value: bool,
    pub await_promise: bool,
    pub timeout_ms: u64,
}

/// CDP `Runtime.callFunctionOn` — used by SelectVerb to invoke a
/// `<select>` element's value setter on a remote object handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeCallFunctionOn {
    pub object_id: String,
    pub function_declaration: String,
    pub arguments_json: String,
    pub return_by_value: bool,
}

/// CDP `DOM.querySelector`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DomQuerySelector {
    pub node_id: u64,
    pub selector: String,
}

/// CDP `DOM.getDocument` — used by SnapshotVerb / NavigateVerb.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DomGetDocument {
    pub depth: i32,
    pub pierce: bool,
}

/// CDP `Page.captureScreenshot`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PageCaptureScreenshot {
    /// "png" | "jpeg".
    pub format: String,
    /// Quality only valid for jpeg; integer 0-100. None → default.
    pub quality: Option<u32>,
    pub capture_beyond_viewport: bool,
}

/// Tagged enum the encoder accepts. One variant per CDP method we issue
/// from any verb. Encoding is method-name-prefixed so the chromium shim
/// can dispatch by tag without parsing CDP-style nested JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CdpMessage {
    PageNavigate(PageNavigate),
    PageAddScriptToEvaluateOnNewDocument(PageAddScriptToEvaluateOnNewDocument),
    InputDispatchMouseEvent(InputDispatchMouseEvent),
    InputDispatchKeyEvent(InputDispatchKeyEvent),
    RuntimeEvaluate(RuntimeEvaluate),
    RuntimeCallFunctionOn(RuntimeCallFunctionOn),
    DomQuerySelector(DomQuerySelector),
    DomGetDocument(DomGetDocument),
    PageCaptureScreenshot(PageCaptureScreenshot),
}

impl CdpMessage {
    /// CDP method name as understood by the chromium shim (e.g.
    /// `"Page.navigate"`). Stable wire identifier.
    pub fn method_name(&self) -> &'static str {
        match self {
            Self::PageNavigate(_) => "Page.navigate",
            Self::PageAddScriptToEvaluateOnNewDocument(_) => {
                "Page.addScriptToEvaluateOnNewDocument"
            }
            Self::InputDispatchMouseEvent(_) => "Input.dispatchMouseEvent",
            Self::InputDispatchKeyEvent(_) => "Input.dispatchKeyEvent",
            Self::RuntimeEvaluate(_) => "Runtime.evaluate",
            Self::RuntimeCallFunctionOn(_) => "Runtime.callFunctionOn",
            Self::DomQuerySelector(_) => "DOM.querySelector",
            Self::DomGetDocument(_) => "DOM.getDocument",
            Self::PageCaptureScreenshot(_) => "Page.captureScreenshot",
        }
    }
}

/// Stateless encoder.
pub struct CdpMessageEncoder;

/// Internal envelope for stable CBOR wire shape: `{"method": "...", "params": {...}}`.
#[derive(Serialize)]
struct CdpEnvelope<'a, T: Serialize> {
    method: &'a str,
    params: &'a T,
}

impl CdpMessageEncoder {
    /// Encode a typed CDP message to CBOR bytes.
    ///
    /// Wire shape (stable):
    /// ```text
    /// CBOR map {
    ///   "method":  text,
    ///   "params":  CBOR map (method-specific)
    /// }
    /// ```
    /// Encoding cannot fail in steady state; OOM during serialization
    /// becomes a wasmtime trap (translated to `SurfaceTrap` receipt by
    /// `loom-host::TrapHandler`, IC-SURF-11).
    pub fn encode(msg: &CdpMessage) -> Vec<u8> {
        let mut buf = Vec::new();
        match msg {
            CdpMessage::PageNavigate(p) => ciborium::ser::into_writer(
                &CdpEnvelope {
                    method: msg.method_name(),
                    params: p,
                },
                &mut buf,
            ),
            CdpMessage::PageAddScriptToEvaluateOnNewDocument(p) => ciborium::ser::into_writer(
                &CdpEnvelope {
                    method: msg.method_name(),
                    params: p,
                },
                &mut buf,
            ),
            CdpMessage::InputDispatchMouseEvent(p) => ciborium::ser::into_writer(
                &CdpEnvelope {
                    method: msg.method_name(),
                    params: p,
                },
                &mut buf,
            ),
            CdpMessage::InputDispatchKeyEvent(p) => ciborium::ser::into_writer(
                &CdpEnvelope {
                    method: msg.method_name(),
                    params: p,
                },
                &mut buf,
            ),
            CdpMessage::RuntimeEvaluate(p) => ciborium::ser::into_writer(
                &CdpEnvelope {
                    method: msg.method_name(),
                    params: p,
                },
                &mut buf,
            ),
            CdpMessage::RuntimeCallFunctionOn(p) => ciborium::ser::into_writer(
                &CdpEnvelope {
                    method: msg.method_name(),
                    params: p,
                },
                &mut buf,
            ),
            CdpMessage::DomQuerySelector(p) => ciborium::ser::into_writer(
                &CdpEnvelope {
                    method: msg.method_name(),
                    params: p,
                },
                &mut buf,
            ),
            CdpMessage::DomGetDocument(p) => ciborium::ser::into_writer(
                &CdpEnvelope {
                    method: msg.method_name(),
                    params: p,
                },
                &mut buf,
            ),
            CdpMessage::PageCaptureScreenshot(p) => ciborium::ser::into_writer(
                &CdpEnvelope {
                    method: msg.method_name(),
                    params: p,
                },
                &mut buf,
            ),
        }
        .expect("CBOR serialization is infallible for fixed-shape CDP structs");
        buf
    }
}

/// Det-init script source. Constant string injected via
/// `Page.AddScriptToEvaluateOnNewDocument` before any DOM observation
/// (SR-SURF-03). Overrides `Date.now`, `Math.random`,
/// `requestAnimationFrame`, `crypto.getRandomValues`, `performance.now`.
///
/// The actual script source is embedded at build time via
/// `include_str!("det_init.js")`. The constant identity (not the source)
/// is what the contract pins.
pub const DET_INIT_JS_NAME: &str = "loom_det_init.js";
