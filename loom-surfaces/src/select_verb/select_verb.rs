// SelectVerb — implements `web-surface::select`.
//
// # Contract semantics
// - **Tier:** hash-only (IC-SURF-07 row `select`).
// - **CDP methods:** `DOM.querySelector` to find the `<select>` element,
//   then `Runtime.callFunctionOn` to set its value and dispatch a
//   synthetic `change` event. Two host::shim_call invocations.
// - **Why callFunctionOn (not dispatchKeyEvent):** native HTML select
//   value cannot be set via key events alone; the controlled-component
//   pattern requires a property assignment + change event.

extern crate alloc;

use alloc::string::String;

use crate::error_mapper::error_mapper::HostError;
use crate::receipt_builder::receipt_builder::Receipt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectAction {
    pub action_id: String,
    pub selector: String,
    /// The `<option>` value to select.
    pub value: String,
    pub timeout_ticks: u64,
}

pub struct SelectVerb;

impl SelectVerb {
    pub fn execute(action: SelectAction) -> Result<Receipt, HostError> {
        use crate::cdp_message_encoder::cdp_message_encoder::{
            CdpMessage, CdpMessageEncoder, DomGetDocument, DomQuerySelector, PageCaptureScreenshot,
            RuntimeCallFunctionOn,
        };
        use crate::error_mapper::error_mapper::{ErrorMapper, SurfaceContext};
        use crate::host_bindings::host_bindings::host;
        use crate::receipt_builder::receipt_builder::{ReceiptBuilder, ReceiptInputs, VerbKind};
        use alloc::collections::BTreeMap;

        let t_start = host::clock_now();
        let action_id = action.action_id.clone();

        let inner = || -> Result<Receipt, HostError> {
            // DOM.querySelector — locate the <select> element (node_id=0 = root)
            let selector_bytes = host::shim_call(
                "chromium",
                &CdpMessageEncoder::encode(&CdpMessage::DomQuerySelector(DomQuerySelector {
                    node_id: 0,
                    selector: action.selector.clone(),
                })),
            )?;
            // Treat shim response as opaque object reference; hex-encode it
            let object_id = {
                let mut hex = alloc::string::String::with_capacity(selector_bytes.len() * 2);
                for b in &selector_bytes {
                    hex.push(char::from_digit((b >> 4) as u32, 16).unwrap_or('0'));
                    hex.push(char::from_digit((b & 0xf) as u32, 16).unwrap_or('0'));
                }
                hex
            };

            // Runtime.callFunctionOn — set value + dispatch change event
            let arguments_json = {
                let mut s = alloc::string::String::with_capacity(action.value.len() + 4);
                s.push_str("[\"");
                s.push_str(&action.value);
                s.push_str("\"]");
                s
            };
            host::shim_call(
                "chromium",
                &CdpMessageEncoder::encode(&CdpMessage::RuntimeCallFunctionOn(
                    RuntimeCallFunctionOn {
                        object_id,
                        function_declaration:
                            "function(v){this.value=v;this.dispatchEvent(new Event('change',{bubbles:true}))}"
                                .into(),
                        arguments_json,
                        return_by_value: true,
                    },
                )),
            )?;

            // Hash-only DOM + screenshot after selection
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
                VerbKind::Select,
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
                    VerbKind::Select,
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
