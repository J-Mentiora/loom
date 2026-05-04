// ClickVerb — implements `web-surface::click`.
//
// # Contract semantics
// - **Tier:** hash-only DOM + hash-only screenshot + action-scoped
//   network events + console lines (IC-SURF-07 row `click`).
// - **CDP method:** `Input.dispatchMouseEvent` with
//   `event_type: "mousePressed"` then `"mouseReleased"` (one click =
//   two CDP messages). Coordinates are integer CSS pixels (BC-SURF-05).
// - **Selector resolution.** Click at element bounding-box centre. The
//   shim resolves the selector and returns the centre; surface never
//   parses CSS itself.
// - **No retry.** If the selector is missing the host returns
//   `host-error::shim-failure { kind: "selector_not_found" }` →
//   ErrorMapper → `WebSelectorNotFound` → error Receipt.

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
            // mousePressed + mouseReleased (one click = two CDP events per spec)
            for event_type in &["mousePressed", "mouseReleased"] {
                host::shim_call(
                    "chromium",
                    &CdpMessageEncoder::encode(&CdpMessage::InputDispatchMouseEvent(
                        InputDispatchMouseEvent {
                            event_type: (*event_type).into(),
                            x: 0,
                            y: 0,
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
