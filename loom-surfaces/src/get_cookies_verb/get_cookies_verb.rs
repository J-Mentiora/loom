// GetCookiesVerb — implements `web-surface::get_cookies` (v0.9.6).
//
// # Contract semantics
// - **Tier:** cookie-result only (no DOM, no screenshot, no network).
//   `ReceiptBuilder::build_cookies_receipt`.
// - **Optional URL filter** — passes through to CDP
//   `Network.getCookies({urls})`. `None` reads all cookies in the
//   active jar.
// - **Raw values per D7.** Operator-facing receipt includes cookie
//   `value` fields verbatim — this verb's purpose is grant inspection
//   and replay-fidelity. Structured logs are scrubbed via
//   `mcp_observability` JSONPaths (§6).
// - **No validation, no vault interaction.** Read path — the cap and
//   per-cookie checks in `validate_cookie_params` apply only to the
//   set path.

extern crate alloc;

use crate::safety::safety::SafetyProfile;
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetCookiesAction {
    pub action_id: String,
    /// Optional URL filter — passes through to CDP `Network.getCookies(urls)`.
    pub urls: Option<Vec<String>>,
    pub timeout_ticks: u64,
    pub profile: SafetyProfile,
    /// v0.9.6: session-context tracking (parity with set/delete/clear).
    /// Unused by the verb today — `get_cookies` is session-scoped
    /// implicitly via the chromium target. `#[serde(default)]` keeps
    /// existing v0.9.5 actions deserialisable.
    #[serde(default)]
    pub session_id: String,
}

use crate::error_mapper::error_mapper::HostError;
use crate::receipt_builder::receipt_builder::Receipt;

/// CDP `Network.getCookies` response shape: `{"cookies": [NetworkCookie, ...]}`.
/// The chromium shim returns CBOR-encoded result objects; deserialising
/// directly to this struct gives us the typed cookie array.
#[derive(Deserialize)]
struct NetworkGetCookiesResponse {
    #[serde(default)]
    cookies: Vec<crate::cookie_types::NetworkCookie>,
}

/// Stateless verb. The single public function `execute` is what
/// `GuestBindings::WebSurfaceImpl::get_cookies` delegates to.
pub struct GetCookiesVerb;

impl GetCookiesVerb {
    /// Run the get_cookies verb against the given action.
    ///
    /// Encodes a `CdpMessage::NetworkGetCookies { urls }` envelope and
    /// dispatches via `host::shim_call("chromium", ...)`. The shim
    /// response is CBOR-encoded `{"cookies": [...]}` — the verb
    /// deserialises into `Vec<NetworkCookie>` and JSON-encodes it onto
    /// the receipt's `get_cookies_result` field.
    pub fn execute(action: GetCookiesAction) -> Result<Receipt, HostError> {
        use crate::cdp_message_encoder::cdp_message_encoder::{
            CdpMessage, CdpMessageEncoder, NetworkGetCookies,
        };
        use crate::error_mapper::error_mapper::{ErrorMapper, SurfaceContext};
        use crate::host_bindings::host_bindings::host;
        use crate::receipt_builder::receipt_builder::{ReceiptBuilder, ReceiptInputs, VerbKind};
        use crate::safety::safety::SafetyPolicy;
        use alloc::collections::BTreeMap;

        let t_start = host::clock_now();
        let action_id = action.action_id.clone();

        // Verb-level safety stub (always-Ok in v0.9.6).
        let _ = SafetyPolicy::check_get_cookies(action.profile);

        let inner = || -> Result<Receipt, HostError> {
            // Build + dispatch CDP Network.getCookies.
            let resp_bytes = host::shim_call(
                "chromium",
                &CdpMessageEncoder::encode(&CdpMessage::NetworkGetCookies(NetworkGetCookies {
                    urls: action.urls.clone(),
                })),
            )?;

            // Decode CBOR response into `{cookies: [...]}` shape.
            let resp: NetworkGetCookiesResponse = ciborium::de::from_reader(resp_bytes.as_slice())
                .map_err(|e| HostError::Internal {
                    reason: alloc::format!("Network.getCookies response decode: {e}"),
                })?;

            // JSON-encode the cookie array for the receipt. Raw values
            // are included here per D7 — the verb is an operator-facing
            // inspection path. The standard `Serialize` impl on
            // `Redacted<T>` emits `"[REDACTED]"` (which is correct for
            // set_cookies + log paths); for this read-side receipt we
            // build the JSON manually so the operator sees what the
            // browser jar actually contains. Structured logs (`tracing`
            // events + MCP `tools/call` mirrors) re-scrub via
            // mcp_observability JSONPaths (§6).
            let cookies_with_raw_values: alloc::vec::Vec<serde_json::Value> = resp
                .cookies
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "name": c.name,
                        "value": c.value.expose(),
                        "domain": c.domain,
                        "path": c.path,
                        "expires": c.expires,
                        "size": c.size,
                        "httpOnly": c.http_only,
                        "secure": c.secure,
                        "session": c.session,
                        "sameSite": c.same_site,
                        "priority": c.priority,
                        "sourceScheme": c.source_scheme,
                        "sourcePort": c.source_port,
                        "partitionKey": c.partition_key,
                        "partitionKeyOpaque": c.partition_key_opaque,
                    })
                })
                .collect();
            let cookies_json = serde_json::to_string(&cookies_with_raw_values).map_err(|e| {
                HostError::Internal {
                    reason: alloc::format!("get_cookies_result serialise: {e}"),
                }
            })?;

            let t_end = host::clock_now();
            Ok(ReceiptBuilder::build_cookies_receipt(
                VerbKind::GetCookies,
                ReceiptInputs {
                    action_id: action.action_id.clone(),
                    timing_ticks: t_end.ticks.saturating_sub(t_start.ticks),
                    get_cookies_result: Some(cookies_json),
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
                    VerbKind::GetCookies,
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
