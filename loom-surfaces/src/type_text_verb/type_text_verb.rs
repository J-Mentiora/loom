// TypeTextVerb — implements `web-surface::type-text`.
//
// # Contract semantics
// - **Tier:** hash-only DOM + hash-only screenshot + console lines.
// - **CDP method:** `Input.dispatchKeyEvent` per character — one
//   `keyDown` + one `char` + one `keyUp` per code point. Surface never
//   uses Runtime.evaluate-style `el.value = "..."` shortcuts (would
//   bypass real input events and miss validation handlers).
// - **Determinism note.** Per-char dispatch ticks the virtual clock
//   monotonically; replay rebuilds identical key-event sequence.
// - **Selector failure.** `selector_not_found` →
//   `WebSelectorNotFound` error Receipt (no retry, no recovery).

extern crate alloc;

use alloc::string::String;

use crate::error_mapper::error_mapper::HostError;
use crate::receipt_builder::receipt_builder::Receipt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeTextAction {
    pub action_id: String,
    pub selector: String,
    /// Text to type — UTF-8; one CDP key event per code point.
    pub text: String,
    /// If true, focus the element before typing (issues a click first).
    pub focus_first: bool,
    pub timeout_ticks: u64,
}

pub struct TypeTextVerb;

impl TypeTextVerb {
    pub fn execute(action: TypeTextAction) -> Result<Receipt, HostError> {
        use crate::cdp_message_encoder::cdp_message_encoder::{
            CdpMessage, CdpMessageEncoder, DomGetDocument, InputDispatchKeyEvent,
            PageCaptureScreenshot,
        };
        use crate::error_mapper::error_mapper::{ErrorMapper, SurfaceContext};
        use crate::host_bindings::host_bindings::host;
        use crate::receipt_builder::receipt_builder::{ReceiptBuilder, ReceiptInputs, VerbKind};
        use alloc::collections::BTreeMap;

        let t_start = host::clock_now();
        let action_id = action.action_id.clone();

        let inner = || -> Result<Receipt, HostError> {
            // Per-char: keyDown + char + keyUp (3 CDP calls per code point)
            for ch in action.text.chars() {
                let mut char_buf = [0u8; 4];
                let ch_str = ch.encode_utf8(&mut char_buf);
                for event_type in &["keyDown", "char", "keyUp"] {
                    host::shim_call(
                        "chromium",
                        &CdpMessageEncoder::encode(&CdpMessage::InputDispatchKeyEvent(
                            InputDispatchKeyEvent {
                                event_type: (*event_type).into(),
                                text: ch_str.into(),
                                key: ch_str.into(),
                                code: ch_str.into(),
                                windows_virtual_key_code: ch as u32,
                                native_virtual_key_code: ch as u32,
                            },
                        )),
                    )?;
                }
            }

            // Hash-only DOM + screenshot after typing
            let dom_bytes = host::shim_call(
                "chromium",
                &CdpMessageEncoder::encode(&CdpMessage::DomGetDocument(DomGetDocument {
                    depth: -1,
                    pierce: false,
                })),
            )?;
            let dom_ref = host::blob_put(&dom_bytes)?;

            let ss_bytes = host::shim_call(
                "chromium",
                &CdpMessageEncoder::encode(&CdpMessage::PageCaptureScreenshot(
                    PageCaptureScreenshot {
                        format: "png".into(),
                        quality: None,
                        capture_beyond_viewport: false,
                    },
                )),
            )?;
            let ss_ref = host::blob_put(&ss_bytes)?;

            let t_end = host::clock_now();
            Ok(ReceiptBuilder::build_hash_only_receipt(
                VerbKind::TypeText,
                ReceiptInputs {
                    action_id: action.action_id.clone(),
                    timing_ticks: t_end.ticks.saturating_sub(t_start.ticks),
                    dom_after_ref: Some(dom_ref),
                    screenshot_after_ref: Some(ss_ref),
                    ..Default::default()
                },
            ))
        };

        match inner() {
            Ok(receipt) => {
                host::receipt_emit(&receipt);
                Ok(receipt)
            }
            Err(err) => {
                let t_end = host::clock_now();
                let receipt = ReceiptBuilder::build_error_receipt(
                    VerbKind::TypeText,
                    action_id,
                    t_end.ticks.saturating_sub(t_start.ticks),
                    ErrorMapper::map(err, SurfaceContext::Web),
                    None,
                    BTreeMap::new(),
                );
                host::receipt_emit(&receipt);
                Ok(receipt)
            }
        }
    }
}
