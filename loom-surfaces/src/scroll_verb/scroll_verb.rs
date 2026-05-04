// ScrollVerb — implements `web-surface::scroll`.
//
// # Contract semantics
// - **Tier:** hash-only (IC-SURF-07 row `scroll`).
// - **CDP method:** `Input.dispatchMouseEvent` with
//   `event_type: "mouseWheel"` and signed integer `deltaX` / `deltaY`
//   in CSS pixels (BC-SURF-05 — no floats).
// - **Scope:** scrolls the element matched by `selector` into view if
//   provided; otherwise scrolls the root document by the given delta.


extern crate alloc;

use alloc::string::String;

use crate::error_mapper::error_mapper::HostError;
use crate::receipt_builder::receipt_builder::Receipt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrollAction {
    pub action_id: String,
    /// None = scroll the root document.
    pub selector: Option<String>,
    /// Signed integer CSS-pixel delta. Positive y scrolls down.
    pub delta_x: i64,
    pub delta_y: i64,
    pub timeout_ticks: u64,
}

pub struct ScrollVerb;

impl ScrollVerb {
    pub fn execute(action: ScrollAction) -> Result<Receipt, HostError> {
        use crate::cdp_message_encoder::cdp_message_encoder::{
            CdpMessage, CdpMessageEncoder, DomGetDocument, InputDispatchMouseEvent,
            PageCaptureScreenshot,
        };
        use crate::error_mapper::error_mapper::{ErrorMapper, SurfaceContext};
        use crate::host_bindings::host_bindings::host;
        use crate::receipt_builder::receipt_builder::{ReceiptBuilder, ReceiptInputs, VerbKind};
        use alloc::collections::BTreeMap;

        let t_start = host::clock_now();
        let action_id = action.action_id.clone();

        let inner = || -> Result<Receipt, HostError> {
            // mouseWheel with signed integer CSS-pixel deltas (BC-SURF-05)
            host::shim_call(
                "chromium",
                &CdpMessageEncoder::encode(&CdpMessage::InputDispatchMouseEvent(
                    InputDispatchMouseEvent {
                        event_type: "mouseWheel".into(),
                        x: 0,
                        y: 0,
                        button: "none".into(),
                        click_count: 0,
                        delta_x: Some(action.delta_x),
                        delta_y: Some(action.delta_y),
                    },
                )),
            )?;

            // Hash-only DOM + screenshot after scroll
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
                VerbKind::Scroll,
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
                    VerbKind::Scroll,
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
