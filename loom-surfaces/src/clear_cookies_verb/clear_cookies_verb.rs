// ClearCookiesVerb — implements `web-surface::clear_cookies` (v0.9.6).
//
// # Contract semantics
// - **Tier:** cookie-result only.
// - **Audit-before-CDP (D9 / FND-0050).** The verb must record
//   `CookiesCleared{target_id, session_id, count_before}` BEFORE
//   issuing `Network.clearBrowserCookies`. `count_before` comes from a
//   synchronous `Network.getCookies` peek so the audit captures the
//   pre-clear count even if the subsequent clear call fails. The
//   audit-chain write happens host-side (manifest_writer); the verb
//   emits a `log_emit("CookiesCleared", {session_id, count_before})`
//   that the host's tracing subscriber routes to the audit writer.
// - **No vault interaction.**

extern crate alloc;

use crate::safety::safety::SafetyProfile;
use alloc::string::String;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClearCookiesAction {
    pub action_id: String,
    pub timeout_ticks: u64,
    pub profile: SafetyProfile,
    /// v0.9.6: session-context tracking (used in the audit log).
    #[serde(default)]
    pub session_id: String,
}

use crate::error_mapper::error_mapper::HostError;
use crate::receipt_builder::receipt_builder::Receipt;

/// CDP `Network.getCookies` response — used here to count cookies
/// before the clear so the audit chain can record `count_before`.
#[derive(Deserialize)]
struct PeekResponse {
    #[serde(default)]
    cookies: alloc::vec::Vec<serde_json::Value>,
}

/// Stateless verb. Delegated to from
/// `GuestBindings::WebSurfaceImpl::clear_cookies`.
pub struct ClearCookiesVerb;

impl ClearCookiesVerb {
    /// Run the clear_cookies verb against the given action.
    ///
    /// Sequence:
    ///   1. CDP `Network.getCookies` (no filter) → count_before
    ///   2. `host::log_emit` `CookiesCleared{session_id, count_before}` —
    ///      the host's tracing subscriber appends the audit entry per
    ///      D9 / FND-0050. Emitted BEFORE the destructive CDP call so
    ///      the audit chain remains intact even if step 3 fails.
    ///   3. CDP `Network.clearBrowserCookies`
    ///   4. Receipt: `clear_cookies_result: {cleared_count}`
    pub fn execute(action: ClearCookiesAction) -> Result<Receipt, HostError> {
        use crate::cdp_message_encoder::cdp_message_encoder::{
            CdpMessage, CdpMessageEncoder, NetworkClearBrowserCookies, NetworkGetCookies,
        };
        use crate::cookie_types::ClearCookiesResult;
        use crate::error_mapper::error_mapper::{ErrorMapper, SurfaceContext};
        use crate::host_bindings::host_bindings::{host, LogLevel};
        use crate::receipt_builder::receipt_builder::{ReceiptBuilder, ReceiptInputs, VerbKind};
        use crate::safety::safety::SafetyPolicy;
        use alloc::collections::BTreeMap;
        use alloc::vec::Vec;

        let t_start = host::clock_now();
        let action_id = action.action_id.clone();

        let _ = SafetyPolicy::check_clear_cookies(action.profile);

        let inner = || -> Result<Receipt, HostError> {
            // STEP 1: synchronous getCookies peek for count_before.
            let peek_bytes = host::shim_call(
                "chromium",
                &CdpMessageEncoder::encode(&CdpMessage::NetworkGetCookies(
                    NetworkGetCookies::default(),
                )),
            )?;
            let peek: PeekResponse =
                ciborium::de::from_reader(peek_bytes.as_slice()).map_err(|e| {
                    HostError::Internal {
                        reason: alloc::format!("Network.getCookies pre-clear peek decode: {e}"),
                    }
                })?;
            let count_before = peek.cookies.len() as u32;

            // STEP 2: audit log BEFORE the destructive CDP call (D9 / FND-0050).
            // The host's tracing subscriber routes this to manifest_writer
            // as an AuditKind::CookiesCleared entry.
            host::log_emit(
                LogLevel::Info,
                "CookiesCleared",
                &[
                    ("session_id".into(), action.session_id.clone()),
                    ("count_before".into(), alloc::format!("{count_before}")),
                ],
            );

            // STEP 3: destructive clear.
            let _ = host::shim_call(
                "chromium",
                &CdpMessageEncoder::encode(&CdpMessage::NetworkClearBrowserCookies(
                    NetworkClearBrowserCookies::default(),
                )),
            )?;

            // STEP 4: receipt.
            let result = ClearCookiesResult {
                cleared_count: count_before,
            };
            let result_json = serde_json::to_string(&result).map_err(|e| HostError::Internal {
                reason: alloc::format!("clear_cookies_result serialise: {e}"),
            })?;

            let t_end = host::clock_now();
            // Touch the Vec import so the build doesn't drop it under unused-imports;
            // alloc::vec::Vec is already imported by the receipt builder transitively
            // but explicit import keeps the verb self-contained.
            let _: Vec<u8> = Vec::new();

            Ok(ReceiptBuilder::build_cookies_receipt(
                VerbKind::ClearCookies,
                ReceiptInputs {
                    action_id: action.action_id.clone(),
                    timing_ticks: t_end.ticks.saturating_sub(t_start.ticks),
                    clear_cookies_result: Some(result_json),
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
                    VerbKind::ClearCookies,
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
