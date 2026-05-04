// WaitVerb — implements `web-surface::wait`.
//
// # Contract semantics
// - **Tier:** hash-only screenshot + console lines, no DOM blob
//   (IC-SURF-07 row `wait`).
// - **Polling loop.** Internally calls `host::shim_call("chromium",
//   Runtime.evaluate(predicate))` repeatedly until the JS predicate
//   evaluates truthy or `timeout_ticks` elapses (measured via
//   bracketing `host::clock_now` reads).
// - **Determinism.** Polling cadence is virtual-clock-driven; sleeps
//   between polls are synthesised by the host (no `std::thread::sleep`
//   in surface — denied by `cargo-deny`).
// - **Timeout.** Elapsed >= `timeout_ticks` → emit error Receipt with
//   `LoomErrorCode::WebActionTimeout`.

extern crate alloc;

use alloc::string::String;

use crate::error_mapper::error_mapper::HostError;
use crate::receipt_builder::receipt_builder::Receipt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitAction {
    pub action_id: String,
    /// JavaScript expression evaluated each poll. Must be side-effect-
    /// free; the chromium shim guards via Runtime.evaluate `throwOnSideEffect`.
    pub predicate_js: String,
    pub timeout_ticks: u64,
    /// Poll period in virtual ticks; default 100.
    pub poll_interval_ticks: u64,
}

pub struct WaitVerb;

impl WaitVerb {
    pub fn execute(action: WaitAction) -> Result<Receipt, HostError> {
        use crate::cdp_message_encoder::cdp_message_encoder::{
            CdpMessage, CdpMessageEncoder, PageCaptureScreenshot, RuntimeEvaluate,
        };
        use crate::error_mapper::error_mapper::{ErrorMapper, ShimFailureKind, SurfaceContext};
        use crate::host_bindings::host_bindings::host;
        use crate::receipt_builder::receipt_builder::{ReceiptBuilder, ReceiptInputs, VerbKind};
        use alloc::collections::BTreeMap;

        let t_start = host::clock_now();
        let action_id = action.action_id.clone();

        let inner = || -> Result<Receipt, HostError> {
            let predicate_msg =
                CdpMessageEncoder::encode(&CdpMessage::RuntimeEvaluate(RuntimeEvaluate {
                    expression: action.predicate_js.clone(),
                    return_by_value: true,
                    await_promise: false,
                    timeout_ms: action.poll_interval_ticks,
                }));

            // Virtual-clock-driven poll loop; host synthesises poll cadence
            loop {
                let result = host::shim_call("chromium", &predicate_msg)?;
                let now = host::clock_now();
                let elapsed = now.ticks.saturating_sub(t_start.ticks);

                // Truthy: at least one non-zero byte in result
                if !result.is_empty() && result.iter().any(|&b| b != 0) {
                    break;
                }

                if elapsed >= action.timeout_ticks {
                    return Err(HostError::ShimFailure {
                        kind: ShimFailureKind::Timeout,
                    });
                }
            }

            // Screenshot-only tier (IC-SURF-07 row wait): no DOM blob
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
                VerbKind::Wait,
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
                    VerbKind::Wait,
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
