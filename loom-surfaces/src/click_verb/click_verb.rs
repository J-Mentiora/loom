// ClickVerb — implements `web-surface::click`.
//
// # Contract semantics
// - **Tier:** hash-only DOM + hash-only screenshot + action-scoped
//   network events + console lines (IC-SURF-07 row `click`).
// - **CDP method:** `Input.dispatchMouseEvent` with
//   `event_type: "mousePressed"` then `"mouseReleased"` (one click =
//   two CDP messages). Coordinates are integer CSS pixels (BC-SURF-05).
// - **Selector resolution.** The verb resolves the selector to the
//   element's bounding-box centre via `hit_test::resolve_centre_for_selector`
//   (DOM.getDocument → DOM.querySelector → DOM.scrollIntoViewIfNeeded →
//   DOM.getBoxModel → centre, rounded to integer CSS pixels). Both
//   mouse events fire at the same coordinates so React's synthetic
//   event system observes a real click, not a 0,0 phantom.
// - **No retry.** Selector missing → `WebSelectorNotFound`. Element
//   exists but has no usable hit-test geometry (display:none, zero
//   area, etc.) → `WebHitTestFailed`.

extern crate alloc;

use alloc::string::String;

use crate::error_mapper::error_mapper::HostError;
use crate::receipt_builder::receipt_builder::Receipt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClickAction {
    pub action_id: String,
    pub selector: String,
    /// "left" | "right" | "middle"; default "left".
    pub button: String,
    pub click_count: u32,
    pub timeout_ticks: u64,
}

pub struct ClickVerb;

impl ClickVerb {
    /// Run the click verb. Hash-only Receipt tier on success; error
    /// Receipt on host-fn failure (per IC-SURF-09 path).
    pub fn execute(action: ClickAction) -> Result<Receipt, HostError> {
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
            // Resolve selector → bounding-box centre (DOM.getDocument →
            // DOM.querySelector → DOM.scrollIntoViewIfNeeded → DOM.getBoxModel).
            let (centre_x, centre_y) =
                crate::hit_test::hit_test::resolve_centre_for_selector(&action.selector)?;

            // mousePressed + mouseReleased at the centre (one click = two
            // CDP events per spec). Both events use the SAME coordinates
            // so the browser dispatches a real click event, not a
            // mouse-down at one point and mouse-up at another.
            for event_type in &["mousePressed", "mouseReleased"] {
                host::shim_call(
                    "chromium",
                    &CdpMessageEncoder::encode(&CdpMessage::InputDispatchMouseEvent(
                        InputDispatchMouseEvent {
                            event_type: (*event_type).into(),
                            x: centre_x,
                            y: centre_y,
                            button: action.button.clone(),
                            click_count: action.click_count,
                            delta_x: None,
                            delta_y: None,
                        },
                    )),
                )?;
            }
            // DOM hash (IC-SURF-07 hash-only tier)
            let dom_bytes = host::shim_call(
                "chromium",
                &CdpMessageEncoder::encode(&CdpMessage::DomGetDocument(DomGetDocument {
                    depth: -1,
                    pierce: false,
                })),
            )?;
            let dom_ref = host::blob_put(&dom_bytes)?;
            // Screenshot hash
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
                VerbKind::Click,
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
                    VerbKind::Click,
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
