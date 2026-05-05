// SnapshotVerb — implements `web-surface::snapshot`.
//
// # Contract semantics
// - **Tier:** full DOM blob + full screenshot. No network events
//   (snapshot is a pure observation; nothing navigates).
// - **CDP methods:** `DOM.getDocument { depth: -1, pierce: true }` to
//   capture the full subtree, then `Page.captureScreenshot` for the
//   visual.
// - **Hashing.** DOM serialised bytes → `host::blob_put`
//   → `ContentRef`; the ref populates `Receipt.dom_after_ref`. Same for
//   screenshot.

extern crate alloc;

use alloc::string::String;

use crate::error_mapper::error_mapper::HostError;
use crate::receipt_builder::receipt_builder::Receipt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotAction {
    pub action_id: String,
    /// If true, also capture screenshot. Default true; false yields
    /// DOM-only snapshot.
    pub include_screenshot: bool,
    pub timeout_ticks: u64,
}

pub struct SnapshotVerb;

impl SnapshotVerb {
    pub fn execute(action: SnapshotAction) -> Result<Receipt, HostError> {
        use crate::cdp_message_encoder::cdp_message_encoder::{
            CdpMessage, CdpMessageEncoder, DomGetDocument, PageCaptureScreenshot,
        };
        use crate::error_mapper::error_mapper::{ErrorMapper, SurfaceContext};
        use crate::host_bindings::host_bindings::host;
        use crate::receipt_builder::receipt_builder::{ReceiptBuilder, ReceiptInputs, VerbKind};
        use alloc::collections::BTreeMap;

        let t_start = host::clock_now();
        let action_id = action.action_id.clone();

        let inner = || -> Result<Receipt, HostError> {
            // DOM.getDocument — full subtree with pierce for shadow DOM
            let dom_bytes = host::shim_call(
                "chromium",
                &CdpMessageEncoder::encode(&CdpMessage::DomGetDocument(DomGetDocument {
                    depth: -1,
                    pierce: true,
                })),
            )?;
            let dom_ref = host::blob_put(&dom_bytes)?;

            let ss_ref = if action.include_screenshot {
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
                Some(host::blob_put(&ss_bytes)?)
            } else {
                None
            };

            let t_end = host::clock_now();
            Ok(ReceiptBuilder::build_full_blob_receipt(
                VerbKind::Snapshot,
                ReceiptInputs {
                    action_id: action.action_id.clone(),
                    timing_ticks: t_end.ticks.saturating_sub(t_start.ticks),
                    dom_after_ref: Some(dom_ref),
                    screenshot_after_ref: ss_ref,
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
                    VerbKind::Snapshot,
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
