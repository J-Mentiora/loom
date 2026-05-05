// module_kind: wit-bindgen-generated
//
// HostBindings — `wit-bindgen` Rust guest-side import stubs for the
// `host` interface declared in `wit/loom-surface.wit`. **This file is
// machine-generated at build time.** The real surface crate's
// `OUT_DIR/bindings.rs` shadows this hand-written shape; we keep this
// file as the human-readable contract surface for review.
//
// # wit-bindgen invocation (emitted by build.rs)
//
// ```ignore
// wit_bindgen::generate!({
//     world: "surface",
//     path: "../../wit/loom-surface.wit",
//     additional_derives: [Debug, Clone, PartialEq, Eq],
// });
// ```
//
// # Contract semantics
// - **8 host-fns and only 8** (matching `wit/loom-surface.wit::interface host`):
//   `clock_now`, `rng_next_u64`, `blob_put`, `blob_get`, `net_request`,
//   `shim_call`, `log_emit`, `receipt_emit`. Adding a 9th host-fn requires
//   a WIT change and a regen — IC-SURF-12.
// - **No hand-rolled `extern "C"` declarations** — IC-SURF-12. Surface
//   crate has zero `unsafe extern "C"` blocks; CI lint
//   `tools/lint-surface-bindings.py` greps and fails on any match.
// - **Synchronous trampolines.** Each function blocks on host return per
//   binding constraint §3.
// - **Result-typed.** Fallible host-fns return `Result<T, HostError>`;
//   infallible (`clock_now`, `rng_next_u64`, `log_emit`, `receipt_emit`)
//   return their bare type or `()`.
// - **HostBindings depends on ErrorMapper** for the WIT
//   `result<T, host-error>` → Rust `Result<T, HostError>` translation
//   inside the generated trampolines (per design.md §2 dependency block).
//
// # Banned in this module (BC-SURF-04)
// - `std::time`, `std::net`, `getrandom`, `std::fs::write` — the whole
//   point of HostBindings is that it is the SOLE legitimate path to time,
//   net, RNG, and storage from inside WASM.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use crate::error_mapper::error_mapper::HostError;
use crate::receipt_builder::receipt_builder::{ContentRef, Receipt};

// ---------------------------------------------------------------------
// WIT-derived shared types (wit-bindgen output)
// ---------------------------------------------------------------------

/// `instant` — monotonic virtual clock value sourced from
/// `loom-core::DeterminismHarness`. Tape-backed in live mode; tape-read
/// in replay (SR-SURF-05).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Instant {
    pub ticks: u64,
}

/// Log severity for `host::log_emit` (IC-SURF-10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// Vault-mediated network request. `headers` may include
/// `Authorization: Grant <id>` — the host substitutes the real secret
/// inside `Vault::substitute` (binding constraint §2). Surface NEVER
/// holds raw OAuth tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetReq {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

/// Vault-mediated network response. `body_ref` points into CAS via
/// `host.blob_put`; `Authorization` and other secret-bearing headers are
/// stripped by the host before returning to surface (BC-SURF-06).
/// `body_size_bytes` reflects the **decompressed** size (SR-SURF-04).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetResp {
    pub status: u32,
    pub headers: Vec<(String, String)>,
    pub body_ref: ContentRef,
    pub body_size_bytes: u64,
}

// ---------------------------------------------------------------------
// The 8 host-fns (sole imports per IC-SURF-12)
// ---------------------------------------------------------------------

/// Module-level alias for the WIT `host` import surface. Every verb
/// imports this module (e.g. `use crate::host_bindings::host_bindings::host;`).
#[cfg(not(test))]
pub mod host {
    use super::*;

    /// Virtual clock read. Source of all time in surface code (IC-SURF-02).
    pub fn clock_now() -> Instant {
        panic!("wit-bindgen trampoline — replaced at build time")
    }
    /// Seeded RNG read. Source of all randomness in surface code (IC-SURF-03).
    pub fn rng_next_u64() -> u64 {
        panic!("wit-bindgen trampoline — replaced at build time")
    }
    /// Append bytes to CAS. Returns hash + size (IC-SURF-04).
    pub fn blob_put(_bytes: &[u8]) -> Result<ContentRef, HostError> {
        panic!("wit-bindgen trampoline — replaced at build time")
    }
    /// Read bytes from CAS by ref.
    pub fn blob_get(_ref_: &ContentRef) -> Result<Vec<u8>, HostError> {
        panic!("wit-bindgen trampoline — replaced at build time")
    }
    /// Vault-mediated outbound request (IC-SURF-05).
    pub fn net_request(_req: &NetReq) -> Result<NetResp, HostError> {
        panic!("wit-bindgen trampoline — replaced at build time")
    }
    /// Bridge to out-of-process shim (IC-SURF-06).
    pub fn shim_call(_shim_id: &str, _msg: &[u8]) -> Result<Vec<u8>, HostError> {
        panic!("wit-bindgen trampoline — replaced at build time")
    }
    /// Sole logging path (IC-SURF-10).
    pub fn log_emit(_level: LogLevel, _msg: &str, _fields: &[(String, String)]) {
        // no-op trampoline; replaced at build time
    }
    /// Sole Receipt emission path (IC-SURF-09).
    pub fn receipt_emit(_r: &Receipt) {
        // no-op trampoline; replaced at build time
    }
}

// ─── Test support ─────────────────────────────────────────────────────────────
// Replaces host-fn stubs in cfg(test) builds. Verbs call through the
// same `host::*` API; the mock intercepts at the module boundary.
// Each thread_local is reset by `MockHost::new()` at test start.
#[cfg(test)]
pub mod mock_host {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// Recorded host-fn invocations (in order).
    #[derive(Debug, Clone, PartialEq)]
    pub enum HostCall {
        ClockNow,
        BlobPut { size: usize },
        ShimCall { shim_id: String, msg_len: usize },
        ReceiptEmit,
        LogEmit,
    }

    thread_local! {
        static CALLS: RefCell<Vec<HostCall>> = const { RefCell::new(Vec::new()) };
        /// Monotonic counter for clock_now ticks.
        static TICKS: RefCell<u64> = const { RefCell::new(1000) };
        /// Default CBOR response bytes returned by `shim_call` when no
        /// method-specific override is set in `SHIM_RESP_BY_METHOD`.
        static SHIM_RESP: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
        /// Per-method CBOR responses. Keyed by CDP method name decoded
        /// from the request's `method` field. Set via
        /// `setup_method_responses` for tests that exercise the
        /// `hit_test::resolve_centre_for_selector` sequence (which makes
        /// 4+ distinct shim_call invocations per click/hover/scroll).
        static SHIM_RESP_BY_METHOD: RefCell<HashMap<String, Vec<u8>>> = RefCell::new(HashMap::new());
        /// Decoded `Input.dispatchMouseEvent` payloads, in dispatch order.
        /// Lets verb tests assert exact (x, y) coordinates rather than
        /// trusting "no error" as proof the click landed at the centre.
        /// Per Phase-4 council reviewer R2.STEAL: prevents the original
        /// (0,0) bug from regressing silently.
        static MOUSE_DISPATCHES: RefCell<Vec<MouseDispatch>> = const { RefCell::new(Vec::new()) };
        /// Emitted receipt captured by receipt_emit.
        static EMITTED: RefCell<Option<Receipt>> = const { RefCell::new(None) };
    }

    /// One decoded `Input.dispatchMouseEvent` request.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct MouseDispatch {
        pub event_type: String,
        pub x: i64,
        pub y: i64,
        pub button: String,
        pub click_count: u32,
        pub delta_x: Option<i64>,
        pub delta_y: Option<i64>,
    }

    /// Set up the mock for a test. Call at the start of each test.
    /// `shim_response` is used as the default for any shim_call whose
    /// method isn't in `SHIM_RESP_BY_METHOD`.
    pub fn setup(shim_response: Vec<u8>) {
        CALLS.with(|c| c.borrow_mut().clear());
        TICKS.with(|t| *t.borrow_mut() = 1000);
        SHIM_RESP.with(|r| *r.borrow_mut() = shim_response);
        SHIM_RESP_BY_METHOD.with(|m| m.borrow_mut().clear());
        MOUSE_DISPATCHES.with(|m| m.borrow_mut().clear());
        EMITTED.with(|e| *e.borrow_mut() = None);
    }

    /// Set per-method CBOR responses. Call AFTER `setup`. Each entry maps
    /// a CDP method name (e.g. `"DOM.querySelector"`) to the CBOR bytes
    /// that `shim_call` should return when the request carries that
    /// method. Used by Click/Hover/Scroll verb tests to mock the new
    /// `hit_test::resolve_centre_for_selector` 4-call sequence.
    pub fn setup_method_responses(map: HashMap<String, Vec<u8>>) {
        SHIM_RESP_BY_METHOD.with(|m| *m.borrow_mut() = map);
    }

    /// Convenience: register CBOR responses that satisfy the
    /// `hit_test::resolve_centre_for_selector` sequence for an
    /// axis-aligned box `[x1, y1, x2, y2]`. Synthetic nodeId is 100.
    /// Also wires `Page.getLayoutMetrics` for ScrollVerb's None branch
    /// with a viewport of (vw, vh).
    pub fn install_hit_test_box(x1: f64, y1: f64, x2: f64, y2: f64, vw: u64, vh: u64) {
        let mut buf_doc = Vec::new();
        ciborium::ser::into_writer(&serde_json::json!({"root": {"nodeId": 1}}), &mut buf_doc)
            .unwrap();
        let mut buf_qs = Vec::new();
        ciborium::ser::into_writer(&serde_json::json!({"nodeId": 100}), &mut buf_qs).unwrap();
        let mut buf_siv = Vec::new();
        ciborium::ser::into_writer(&serde_json::json!({}), &mut buf_siv).unwrap();
        let mut buf_bm = Vec::new();
        ciborium::ser::into_writer(
            &serde_json::json!({
                "model": {
                    "content": [x1, y1, x2, y1, x2, y2, x1, y2],
                    "width": (x2 - x1) as u64,
                    "height": (y2 - y1) as u64,
                }
            }),
            &mut buf_bm,
        )
        .unwrap();
        let mut buf_lm = Vec::new();
        ciborium::ser::into_writer(
            &serde_json::json!({
                "cssLayoutViewport": {
                    "clientWidth":  vw,
                    "clientHeight": vh,
                }
            }),
            &mut buf_lm,
        )
        .unwrap();
        let mut map = HashMap::new();
        map.insert("DOM.getDocument".to_string(), buf_doc);
        map.insert("DOM.querySelector".to_string(), buf_qs);
        map.insert("DOM.scrollIntoViewIfNeeded".to_string(), buf_siv);
        map.insert("DOM.getBoxModel".to_string(), buf_bm);
        map.insert("Page.getLayoutMetrics".to_string(), buf_lm);
        setup_method_responses(map);
    }

    /// Return recorded host-fn calls in order.
    pub fn calls() -> Vec<HostCall> {
        CALLS.with(|c| c.borrow().clone())
    }

    /// Return every `Input.dispatchMouseEvent` payload the verb under
    /// test sent, in dispatch order. Decoded by `shim_call_impl` from
    /// the typed CDP envelope. Empty if no mouse events were dispatched.
    pub fn mouse_dispatches() -> Vec<MouseDispatch> {
        MOUSE_DISPATCHES.with(|m| m.borrow().clone())
    }

    /// Return the receipt that was emitted via receipt_emit.
    pub fn emitted_receipt() -> Option<Receipt> {
        EMITTED.with(|e| e.borrow().clone())
    }

    // ── Implementations used by verb tests ──────────────────────────
    pub fn clock_now_impl() -> Instant {
        CALLS.with(|c| c.borrow_mut().push(HostCall::ClockNow));
        TICKS.with(|t| {
            let v = *t.borrow();
            *t.borrow_mut() = v + 100;
            Instant { ticks: v }
        })
    }

    pub fn blob_put_impl(bytes: &[u8]) -> Result<ContentRef, HostError> {
        CALLS.with(|c| c.borrow_mut().push(HostCall::BlobPut { size: bytes.len() }));
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(bytes);
        let hash_bytes = h.finalize();
        let sha256_hex = hex_encode(&hash_bytes);
        Ok(ContentRef {
            sha256_hex,
            size_bytes: bytes.len() as u64,
        })
    }

    pub fn shim_call_impl(shim_id: &str, msg: &[u8]) -> Result<Vec<u8>, HostError> {
        CALLS.with(|c| {
            c.borrow_mut().push(HostCall::ShimCall {
                shim_id: shim_id.to_string(),
                msg_len: msg.len(),
            })
        });
        // Decode the CBOR envelope once: needed for per-method response
        // routing AND for capturing Input.dispatchMouseEvent payloads
        // (used by verb tests to assert exact dispatched coordinates).
        if let Ok(env) = ciborium::de::from_reader::<ciborium::value::Value, _>(msg) {
            let method = env.as_map().and_then(|m| {
                m.iter().find_map(|(k, v)| {
                    if k.as_text() == Some("method") {
                        v.as_text().map(String::from)
                    } else {
                        None
                    }
                })
            });
            // Capture Input.dispatchMouseEvent payloads so verb tests
            // can directly assert (x, y) — converts bug-fix verification
            // from circumstantial to direct (Phase-4 council R2.STEAL).
            if method.as_deref() == Some("Input.dispatchMouseEvent") {
                if let Some(params) = env.as_map().and_then(|m| {
                    m.iter().find_map(|(k, v)| {
                        if k.as_text() == Some("params") {
                            Some(v.clone())
                        } else {
                            None
                        }
                    })
                }) {
                    if let Some(p) = decode_mouse_dispatch(&params) {
                        MOUSE_DISPATCHES.with(|d| d.borrow_mut().push(p));
                    }
                }
            }
            if let Some(m) = method {
                if let Some(resp) = SHIM_RESP_BY_METHOD.with(|map| map.borrow().get(&m).cloned()) {
                    return Ok(resp);
                }
            }
        }
        Ok(SHIM_RESP.with(|r| r.borrow().clone()))
    }

    /// Decode a CBOR `Value` matching the `InputDispatchMouseEvent`
    /// param struct shape (see `cdp_message_encoder.rs`). Returns None
    /// if any required field is missing or the wrong type.
    fn decode_mouse_dispatch(params: &ciborium::value::Value) -> Option<MouseDispatch> {
        let map = params.as_map()?;
        let mut event_type: Option<String> = None;
        let mut x: Option<i64> = None;
        let mut y: Option<i64> = None;
        let mut button: Option<String> = None;
        let mut click_count: Option<u32> = None;
        let mut delta_x: Option<i64> = None;
        let mut delta_y: Option<i64> = None;
        for (k, v) in map.iter() {
            match k.as_text()? {
                "event_type" => event_type = v.as_text().map(String::from),
                "x" => x = v.as_integer().and_then(|i| i64::try_from(i).ok()),
                "y" => y = v.as_integer().and_then(|i| i64::try_from(i).ok()),
                "button" => button = v.as_text().map(String::from),
                "click_count" => click_count = v.as_integer().and_then(|i| u32::try_from(i).ok()),
                "delta_x" => delta_x = v.as_integer().and_then(|i| i64::try_from(i).ok()),
                "delta_y" => delta_y = v.as_integer().and_then(|i| i64::try_from(i).ok()),
                _ => {}
            }
        }
        Some(MouseDispatch {
            event_type: event_type?,
            x: x?,
            y: y?,
            button: button?,
            click_count: click_count?,
            delta_x,
            delta_y,
        })
    }

    pub fn receipt_emit_impl(r: &Receipt) {
        CALLS.with(|c| c.borrow_mut().push(HostCall::ReceiptEmit));
        EMITTED.with(|e| *e.borrow_mut() = Some(r.clone()));
    }

    fn hex_encode(bytes: &[u8]) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
        out
    }
}

// ── Overrides used in cfg(test) verb tests ─────────────────────────────────
// When building test binaries, verbs that import `crate::host_bindings::host_bindings::host`
// get these overrides instead of the panic stubs.
// The module is re-exported under the same path so verb code needs no changes.
#[cfg(test)]
pub mod host {
    use super::*;

    pub fn clock_now() -> Instant {
        mock_host::clock_now_impl()
    }

    pub fn rng_next_u64() -> u64 {
        0xdeadbeef_cafebabe
    }

    pub fn blob_put(bytes: &[u8]) -> Result<ContentRef, HostError> {
        mock_host::blob_put_impl(bytes)
    }

    pub fn blob_get(_ref_: &ContentRef) -> Result<Vec<u8>, HostError> {
        Ok(Vec::new())
    }

    pub fn net_request(_req: &NetReq) -> Result<NetResp, HostError> {
        panic!("net_request not mocked in tests")
    }

    pub fn shim_call(shim_id: &str, msg: &[u8]) -> Result<Vec<u8>, HostError> {
        mock_host::shim_call_impl(shim_id, msg)
    }

    pub fn log_emit(_level: LogLevel, _msg: &str, _fields: &[(String, String)]) {}

    pub fn receipt_emit(r: &Receipt) {
        mock_host::receipt_emit_impl(r)
    }
}
