// NavigateVerb — implements `web-surface::navigate`.
//
// # Contract semantics
// - **Tier:** full DOM blob + full screenshot + full network events.
// - **Determinism injection.** STEP 2 calls
//   `host::shim_call("chromium", Page.AddScriptToEvaluateOnNewDocument
//   { source: DET_INIT_JS, run_immediately: true })` BEFORE STEP 3's
//   `Page.Navigate`. Deferred injection (post-page-load) is a KILL.
// - **Two clock_now reads.** STEP 1 captures `t_start`; STEP 5 captures
//   `t_end`; `timing_ticks = t_end - t_start` (integer).
// - **Receipt path.** Final operation is
//   `host::receipt_emit(receipt)`. The `Result<Receipt, HostError>`
//   return preserves the receipt for the WIT boundary too.
// - **No retry, no panic, no `catch_unwind`.** Host-fn errors propagate
//   via `?` to ErrorMapper → error Receipt → `host::receipt_emit`.
//
// Sequence: design.md §3.1.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use crate::error_mapper::error_mapper::HostError;
use crate::receipt_builder::receipt_builder::Receipt;

/// `web-surface::action` carrying navigate-specific parameters.
/// The shared `Action` enum is wit-bindgen output; we shape only the
/// navigate variant here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavigateAction {
    pub action_id: String,
    pub url: String,
    /// Optional referrer; passes through to CDP `Page.navigate`.
    pub referrer: Option<String>,
    /// Per-action wall-clock budget (informational; host enforces).
    pub timeout_ticks: u64,
}

/// Stateless verb. The single public function `execute` is what
/// `GuestBindings::WebSurfaceImpl::navigate` delegates to.
pub struct NavigateVerb;

impl NavigateVerb {
    /// Run the navigate verb against the given action.
    ///
    /// On success: `Ok(Receipt)` with `status = Ok` and full-blob tier
    /// fields (`dom_after_ref`, `screenshot_after_ref`, `network_events`).
    ///
    /// On host-fn failure: `Ok(Receipt)` with `status = Error` and
    /// `error_code` populated by `ErrorMapper::map(host_err, Web)`. The
    /// `Err(HostError)` arm of the WIT result is reserved for cases
    /// where Receipt assembly itself failed (e.g., malformed action
    /// before ReceiptBuilder is reachable).
    ///
    /// The verb's last operation is `host::receipt_emit`;
    /// the returned `Receipt` is also the WIT result payload.
    pub fn execute(action: NavigateAction) -> Result<Receipt, HostError> {
        use crate::cdp_message_encoder::cdp_message_encoder::{
            CdpMessage, CdpMessageEncoder, DomGetDocument, PageAddScriptToEvaluateOnNewDocument,
            PageCaptureScreenshot, PageNavigate, DET_INIT_JS_NAME,
        };
        use crate::error_mapper::error_mapper::{ErrorMapper, SurfaceContext};
        use crate::host_bindings::host_bindings::host;
        use crate::receipt_builder::receipt_builder::{ReceiptBuilder, ReceiptInputs, VerbKind};
        use alloc::collections::BTreeMap;

        let t_start = host::clock_now();
        let action_id = action.action_id.clone();

        let inner = || -> Result<Receipt, HostError> {
            // Inject det_init BEFORE Page.navigate (KILL criterion)
            host::shim_call(
                "chromium",
                &CdpMessageEncoder::encode(&CdpMessage::PageAddScriptToEvaluateOnNewDocument(
                    PageAddScriptToEvaluateOnNewDocument {
                        source: DET_INIT_JS_NAME.into(),
                        run_immediately: true,
                    },
                )),
            )?;
            // Page.navigate
            host::shim_call(
                "chromium",
                &CdpMessageEncoder::encode(&CdpMessage::PageNavigate(PageNavigate {
                    url: action.url.clone(),
                    transition_type: "typed".into(),
                })),
            )?;
            // DOM capture — full blob
            let dom_bytes = host::shim_call(
                "chromium",
                &CdpMessageEncoder::encode(&CdpMessage::DomGetDocument(DomGetDocument {
                    depth: -1,
                    pierce: true,
                })),
            )?;
            let dom_ref = host::blob_put(&dom_bytes)?;
            // Screenshot capture — full blob
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
            Ok(ReceiptBuilder::build_full_blob_receipt(
                VerbKind::Navigate,
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
                    VerbKind::Navigate,
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

/// Inspectable trace of host-fn invocations the verb makes, in order.
/// Exposed for interface testing of the ordering invariant
/// (det_init injection happens BEFORE Page.navigate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavigateStep {
    /// STEP 1: t_start = host::clock_now()
    ClockNow,
    /// STEP 2: host::shim_call("chromium", AddScriptToEvaluateOnNewDocument(det_init.js))
    InjectDetInit,
    /// STEP 3: host::shim_call("chromium", Page.navigate(url))
    PageNavigate,
    /// STEP 4a: host::blob_put(dom_bytes)
    BlobPutDom,
    /// STEP 4b: host::blob_put(screenshot_bytes)
    BlobPutScreenshot,
    /// STEP 5: t_end = host::clock_now()
    ClockNowEnd,
    /// STEP 7: host::receipt_emit(receipt)
    ReceiptEmit,
}

impl NavigateVerb {
    /// Returns the canonical step sequence for the navigate verb.
    /// Defined for interface tests; mirrors design.md §3.1 exactly.
    pub fn canonical_steps() -> Vec<NavigateStep> {
        alloc::vec![
            NavigateStep::ClockNow,
            NavigateStep::InjectDetInit,
            NavigateStep::PageNavigate,
            NavigateStep::BlobPutDom,
            NavigateStep::BlobPutScreenshot,
            NavigateStep::ClockNowEnd,
            NavigateStep::ReceiptEmit,
        ]
    }
}
