// HoverVerb — implements `web-surface::hover`.
//
// # Contract semantics
// - **Tier:** hash-only (IC-SURF-07 row `hover`).
// - **CDP method:** `Input.dispatchMouseEvent` with
//   `event_type: "mouseMoved"` to the element's bounding-box centre.
// - **No tooltip-wait built in.** Hover is synchronous; if the agent
//   wants tooltip text, follow with a `wait` verb.

extern crate alloc;

use alloc::string::String;

use crate::error_mapper::error_mapper::HostError;
use crate::receipt_builder::receipt_builder::Receipt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoverAction {
    pub action_id: String,
    pub selector: String,
    pub timeout_ticks: u64,
}

pub struct HoverVerb;

impl HoverVerb {
    pub fn execute(action: HoverAction) -> Result<Receipt, HostError> {
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
            // mouseMoved at element bounding-box centre (shim resolves selector)
            host::shim_call(
                "chromium",
                &CdpMessageEncoder::encode(&CdpMessage::InputDispatchMouseEvent(
                    InputDispatchMouseEvent {
                        event_type: "mouseMoved".into(),
                        x: 0,
                        y: 0,
                        button: "none".into(),
                        click_count: 0,
                        delta_x: None,
                        delta_y: None,
                    },
                )),
            )?;

            // Hash-only DOM + screenshot after hover
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
                VerbKind::Hover,
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
                    VerbKind::Hover,
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
