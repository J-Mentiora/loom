// DeleteCookiesVerb — implements `web-surface::delete_cookies` (v0.9.6).
//
// # Contract semantics
// - **Tier:** cookie-result only.
// - **Match determination.** The receipt's `matched: bool` is determined
//   by a `Network.getCookies` peek BEFORE and AFTER the destructive
//   `Network.deleteCookies` call. `matched = present_before &&
//   !present_after`. This makes the verb's outcome idempotent and
//   self-evidencing across both "already gone" and "successful delete"
//   cases.
// - **Match semantics.** A cookie is considered the target when its
//   `name` equals the action's `name` AND (if the action supplies them)
//   its `domain` / `path` equal the action's values. Action-supplied
//   `url` is passed through to CDP `Network.deleteCookies` verbatim
//   but is not used in the local match — CDP derives domain/path
//   from `url` server-side.

extern crate alloc;

use crate::safety::safety::SafetyProfile;
use alloc::string::String;
use serde::{Deserialize, Serialize};

/// Single-cookie targeted delete. Maps to CDP `Network.deleteCookies(name, url?, domain?, path?)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteCookiesAction {
    pub action_id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub timeout_ticks: u64,
    pub profile: SafetyProfile,
    /// v0.9.6: session-context tracking.
    #[serde(default)]
    pub session_id: String,
}

use crate::error_mapper::error_mapper::HostError;
use crate::receipt_builder::receipt_builder::Receipt;

#[derive(Deserialize)]
struct PeekResponse {
    #[serde(default)]
    cookies: alloc::vec::Vec<PeekCookie>,
}

#[derive(Deserialize)]
struct PeekCookie {
    name: String,
    #[serde(default)]
    domain: String,
    #[serde(default)]
    path: String,
}

/// Stateless verb.
pub struct DeleteCookiesVerb;

impl DeleteCookiesVerb {
    /// Run the delete_cookies verb against the given action.
    ///
    /// Sequence:
    ///   1. CDP `Network.getCookies` — pre-delete peek
    ///   2. CDP `Network.deleteCookies { name, url, domain, path }`
    ///   3. CDP `Network.getCookies` — post-delete peek
    ///   4. matched = present_before && !present_after
    ///   5. Receipt: `delete_cookies_result: {name, matched}`
    pub fn execute(action: DeleteCookiesAction) -> Result<Receipt, HostError> {
        use crate::cdp_message_encoder::cdp_message_encoder::{
            CdpMessage, CdpMessageEncoder, NetworkDeleteCookies, NetworkGetCookies,
        };
        use crate::cookie_types::DeleteCookiesResult;
        use crate::error_mapper::error_mapper::{ErrorMapper, SurfaceContext};
        use crate::host_bindings::host_bindings::host;
        use crate::receipt_builder::receipt_builder::{ReceiptBuilder, ReceiptInputs, VerbKind};
        use crate::safety::safety::SafetyPolicy;
        use alloc::collections::BTreeMap;

        let t_start = host::clock_now();
        let action_id = action.action_id.clone();

        let _ = SafetyPolicy::check_delete_cookies(action.profile);

        let inner = || -> Result<Receipt, HostError> {
            // Helper: peek the jar and decode.
            let peek = || -> Result<PeekResponse, HostError> {
                let bytes = host::shim_call(
                    "chromium",
                    &CdpMessageEncoder::encode(&CdpMessage::NetworkGetCookies(
                        NetworkGetCookies::default(),
                    )),
                )?;
                ciborium::de::from_reader(bytes.as_slice()).map_err(|e| HostError::Internal {
                    reason: alloc::format!("Network.getCookies peek decode: {e}"),
                })
            };

            // Local match: name eq, domain eq (if supplied), path eq (if supplied).
            let matches_target = |c: &PeekCookie| -> bool {
                if c.name != action.name {
                    return false;
                }
                if let Some(d) = action.domain.as_deref() {
                    if c.domain != d {
                        return false;
                    }
                }
                if let Some(p) = action.path.as_deref() {
                    if c.path != p {
                        return false;
                    }
                }
                true
            };

            // STEP 1: pre-delete peek.
            let before = peek()?;
            let present_before = before.cookies.iter().any(matches_target);

            // STEP 2: destructive delete.
            let _ = host::shim_call(
                "chromium",
                &CdpMessageEncoder::encode(&CdpMessage::NetworkDeleteCookies(
                    NetworkDeleteCookies {
                        name: action.name.clone(),
                        url: action.url.clone(),
                        domain: action.domain.clone(),
                        path: action.path.clone(),
                    },
                )),
            )?;

            // STEP 3: post-delete peek.
            let after = peek()?;
            let present_after = after.cookies.iter().any(matches_target);

            // STEP 4: matched.
            let matched = present_before && !present_after;

            // STEP 5: build receipt.
            let result = DeleteCookiesResult {
                name: action.name.clone(),
                matched,
            };
            let result_json = serde_json::to_string(&result).map_err(|e| HostError::Internal {
                reason: alloc::format!("delete_cookies_result serialise: {e}"),
            })?;

            let t_end = host::clock_now();
            Ok(ReceiptBuilder::build_cookies_receipt(
                VerbKind::DeleteCookies,
                ReceiptInputs {
                    action_id: action.action_id.clone(),
                    timing_ticks: t_end.ticks.saturating_sub(t_start.ticks),
                    delete_cookies_result: Some(result_json),
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
                    VerbKind::DeleteCookies,
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
