// EvaluateVerb — implements `web-surface::evaluate`.
//
// **DEAD CODE NOTICE (AC-SAFEPROF-01..03 fix, 2026-05-03):**
// The production safe-profile evaluate gate now lives at
// `loom-daemon/src/main.rs::WasmBridge::dispatch_action_blocking` —
// the daemon-layer gate operates on the typed `Action::WebEvaluate`
// before host dispatch and is the only path the operator's reproducer
// hits. `EvaluateVerb::execute` below is no longer in the production
// call chain because `loom-surface-web/src/lib.rs:132` calls a generic
// `dispatch("evaluate", a)` that bypasses this verb. Tests in
// `interface_tests.rs` exercise the policy logic in isolation but do
// NOT prove production behavior. Schedule for removal after one
// release cycle confirms no regressions; until then, do not modify
// the safety-policy logic here without also updating the daemon gate.
//
// # Contract semantics
// - **Tier:** console-only + return value (IC-SURF-07 row `evaluate`).
//   No DOM blob, no screenshot, no network events.
// - **CDP method:** `Runtime.evaluate` with `returnByValue: true`.
// - **Return value canonicalisation.** The shim returns a CBOR-encoded
//   serde value; surface stores the canonical-JSON string in
//   `Receipt.evaluate_return_value` (host's ReceiptMarshaller does the
//   final `serde_jcs::to_string`).
// - **Console capture.** All `Runtime.consoleAPICalled` events emitted
//   during the evaluate are folded into `Receipt.console_lines`.

extern crate alloc;

use alloc::string::String;

use crate::error_mapper::error_mapper::HostError;
use crate::receipt_builder::receipt_builder::Receipt;
use crate::safety::safety::SafetyProfile;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluateAction {
    pub action_id: String,
    pub expression: String,
    pub await_promise: bool,
    pub timeout_ticks: u64,
    /// Session safety profile. Set by the host when constructing the action
    /// from `CreateSessionParams::profile`. Defaults to `Default` when the
    /// session was created without an explicit profile.
    pub profile: SafetyProfile,
}

pub struct EvaluateVerb;

impl EvaluateVerb {
    pub fn execute(action: EvaluateAction) -> Result<Receipt, HostError> {
        use crate::cdp_message_encoder::cdp_message_encoder::{
            CdpMessage, CdpMessageEncoder, RuntimeEvaluate,
        };
        use crate::error_mapper::error_mapper::{ErrorMapper, SurfaceContext};
        use crate::host_bindings::host_bindings::host;
        use crate::receipt_builder::receipt_builder::{ReceiptBuilder, ReceiptInputs, VerbKind};
        use alloc::collections::BTreeMap;

        let t_start = host::clock_now();
        let action_id = action.action_id.clone();

        // AC-WEB-07.1: safe profile pre-gate — check expression against denylist
        // before reaching the CDP shim. Early-return error receipt; script NOT run.
        {
            use crate::error_mapper::error_mapper::LoomErrorCode;
            use crate::receipt_builder::receipt_builder::{ReceiptBuilder, VerbKind};
            use crate::safety::safety::SafetyPolicy;
            use alloc::collections::BTreeMap;

            if SafetyPolicy::check_evaluate(action.profile, &action.expression).is_some() {
                let t_end = host::clock_now();
                let receipt = ReceiptBuilder::build_error_receipt(
                    VerbKind::Evaluate,
                    action_id.clone(),
                    t_end.ticks.saturating_sub(t_start.ticks),
                    LoomErrorCode::SchemaViolation,
                    Some("safe_profile_evaluate_denylist_match".into()),
                    BTreeMap::new(),
                );
                host::receipt_emit(&receipt);
                return Ok(receipt);
            }
        }

        let inner = || -> Result<Receipt, HostError> {
            // Runtime.evaluate; shim returns CBOR-encoded result bytes
            let result_bytes = host::shim_call(
                "chromium",
                &CdpMessageEncoder::encode(&CdpMessage::RuntimeEvaluate(RuntimeEvaluate {
                    expression: action.expression.clone(),
                    return_by_value: true,
                    await_promise: action.await_promise,
                    timeout_ms: action.timeout_ticks,
                })),
            )?;
            // Encode result bytes as hex string for canonical receipt field
            let return_value = {
                let mut hex = alloc::string::String::with_capacity(result_bytes.len() * 2);
                for b in &result_bytes {
                    hex.push(char::from_digit((b >> 4) as u32, 16).unwrap_or('0'));
                    hex.push(char::from_digit((b & 0xf) as u32, 16).unwrap_or('0'));
                }
                hex
            };
            let t_end = host::clock_now();
            // Console-only tier (IC-SURF-07): no DOM, no screenshot, no network
            Ok(ReceiptBuilder::build_console_only_receipt(ReceiptInputs {
                action_id: action.action_id.clone(),
                timing_ticks: t_end.ticks.saturating_sub(t_start.ticks),
                evaluate_return_value: Some(return_value),
                ..Default::default()
            }))
        };

        match inner() {
            Ok(receipt) => {
                host::receipt_emit(&receipt);
                Ok(receipt)
            }
            Err(err) => {
                let t_end = host::clock_now();
                let receipt = ReceiptBuilder::build_error_receipt(
                    VerbKind::Evaluate,
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
