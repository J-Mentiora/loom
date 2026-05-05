// ScreenshotVerb — implements `web-surface::screenshot`.
//
// # Contract semantics
// - **Tier:** full screenshot only. No DOM, no network events.
// - **CDP method:** `Page.captureScreenshot` with `format: "png"`
//   default; jpeg quality is integer 0-100 if used.
// - **Hashing.** Screenshot bytes go through `host::blob_put` →
//   `ContentRef`; the ref populates `Receipt.screenshot_after_ref`.
// - **NFR-DET-01.** Screenshots are excluded from the hash chain (see
//   binding-constraints §Hard binding 5); the Receipt itself is hashed
//   via canonical JSON, but the screenshot bytes themselves are not in
//   the hash chain.

extern crate alloc;

use alloc::string::String;

use crate::error_mapper::error_mapper::HostError;
use crate::receipt_builder::receipt_builder::Receipt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenshotAction {
    pub action_id: String,
    /// "png" | "jpeg".
    pub format: String,
    /// Only valid for jpeg; integer 0-100. None = use shim default.
    pub quality: Option<u32>,
    pub capture_beyond_viewport: bool,
    pub timeout_ticks: u64,
}

pub struct ScreenshotVerb;

impl ScreenshotVerb {
    pub fn execute(action: ScreenshotAction) -> Result<Receipt, HostError> {
        use crate::cdp_message_encoder::cdp_message_encoder::{
            CdpMessage, CdpMessageEncoder, PageCaptureScreenshot,
        };
        use crate::error_mapper::error_mapper::{ErrorMapper, SurfaceContext};
        use crate::host_bindings::host_bindings::host;
        use crate::receipt_builder::receipt_builder::{ReceiptBuilder, ReceiptInputs, VerbKind};
        use alloc::collections::BTreeMap;

        let t_start = host::clock_now();
        let action_id = action.action_id.clone();

        let inner = || -> Result<Receipt, HostError> {
            // Page.captureScreenshot → blob_put → content-addressed ref
            let ss_bytes = host::shim_call(
                "chromium",
                &CdpMessageEncoder::encode(&CdpMessage::PageCaptureScreenshot(
                    PageCaptureScreenshot {
                        format: action.format.clone(),
                        quality: action.quality,
                        capture_beyond_viewport: action.capture_beyond_viewport,
                    },
                )),
            )?;
            let ss_ref = host::blob_put(&ss_bytes)?;
            let t_end = host::clock_now();
            Ok(ReceiptBuilder::build_screenshot_only_receipt(
                ReceiptInputs {
                    action_id: action.action_id.clone(),
                    timing_ticks: t_end.ticks.saturating_sub(t_start.ticks),
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
                    VerbKind::Screenshot,
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
