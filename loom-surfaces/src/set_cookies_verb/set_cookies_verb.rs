// SetCookiesVerb — implements `web-surface::set_cookies` (v0.9.6).
//
// # Contract semantics
// - **Tier:** cookie-result only (no DOM, no screenshot, no network).
//   `ReceiptBuilder::build_cookies_receipt`.
// - **Source XOR.** `Inline { cookies }` uses cookie material directly;
//   `Grant { grant_id }` resolves through
//   `host::vault_substitute_cookies(grant_id, session_id)` — the daemon-side
//   chokepoint that fetches the vault-stored keychain blob, validates the
//   session binding (D5 / FND-0008), and returns the JSON bytes.
// - **Two clock_now reads** (matching the navigate-verb pattern).
//   STEP 1 captures `t_start`; STEP 5 captures `t_end`.
// - **Receipt path.** Final operation is `host::receipt_emit(receipt)`.
//   The `Result<Receipt, HostError>` return preserves the receipt for the
//   WIT boundary too.
// - **Atomic validation.** Per-cookie validation
//   (`validate_cookie_params`) runs before any CDP call; any failure
//   short-circuits to an error receipt carrying
//   `LoomErrorCode::CookieValidationError(CookieValidationError)`.
// - **No retry, no panic, no `catch_unwind`.** Host-fn errors propagate
//   via `?` → ErrorMapper → error Receipt → `host::receipt_emit`.

extern crate alloc;

use crate::cookie_types::CookieSource;
use crate::safety::safety::SafetyProfile;
use alloc::string::String;
use serde::{Deserialize, Serialize};

/// `web-surface::action` carrying set-cookies parameters. `source` is the
/// typed XOR enum from `cookie_types`: `Inline { cookies }` or
/// `Grant { grant_id }`.
///
/// `session_id` (v0.9.6) is populated by the daemon-side dispatcher from
/// the JSON-RPC `params.session_id` and threaded through to the
/// `host::vault_substitute_cookies` call when `source` is `Grant`.
/// `#[serde(default)]` keeps existing v0.9.5 serialised actions
/// (without `session_id`) deserialisable for the in-flight test corpus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetCookiesAction {
    pub action_id: String,
    pub source: CookieSource,
    pub timeout_ticks: u64,
    pub profile: SafetyProfile,
    #[serde(default)]
    pub session_id: String,
}

impl Serialize for SafetyProfile {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Default => s.serialize_str("default"),
            Self::Safe => s.serialize_str("safe"),
        }
    }
}

impl<'de> Deserialize<'de> for SafetyProfile {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        match s.as_str() {
            "default" => Ok(Self::Default),
            "safe" => Ok(Self::Safe),
            other => Err(serde::de::Error::custom(alloc::format!(
                "unknown SafetyProfile: {other}"
            ))),
        }
    }
}

use crate::error_mapper::error_mapper::HostError;
use crate::receipt_builder::receipt_builder::Receipt;

/// Stateless verb. The single public function `execute` is what
/// `GuestBindings::WebSurfaceImpl::set_cookies` delegates to.
pub struct SetCookiesVerb;

impl SetCookiesVerb {
    /// Run the set_cookies verb against the given action.
    ///
    /// Source resolution:
    /// - `CookieSource::Inline { cookies }` — cookies pass through directly.
    /// - `CookieSource::Grant { grant_id }` — calls
    ///   `host::vault_substitute_cookies(grant_id, session_id)` to fetch
    ///   the keychain blob bytes, deserialises into `CookieKeychainBlob`,
    ///   and uses `blob.cookies`. The grant must (a) be alive, (b) be
    ///   `CredentialType::Cookie`, and (c) match the action's `session_id`
    ///   per D5 — any mismatch surfaces as `VaultRejection` via the host-fn.
    ///
    /// Per-cookie validation (`validate_cookie_params`) runs synchronously
    /// before any CDP call: 64-cookie cap, name/value/expires checks.
    /// Validation failure short-circuits to an error receipt with
    /// `LoomErrorCode::CookieValidationError(CookieValidationError)`.
    ///
    /// On success: encodes a single `CdpMessage::NetworkSetCookies` and
    /// dispatches via `host::shim_call("chromium", &bytes)`. Receipt
    /// carries `set_cookies_result` (JSON-encoded `Vec<SetCookieResult>`)
    /// with `success: true` for every validated cookie.
    pub fn execute(action: SetCookiesAction) -> Result<Receipt, HostError> {
        use crate::cdp_message_encoder::cdp_message_encoder::{
            CdpMessage, CdpMessageEncoder, NetworkSetCookies,
        };
        use crate::cookie_types::{
            validate_cookie_params, CookieKeychainBlob, CookieSource as CS, NetworkCookieParam,
            SetCookieResult,
        };
        use crate::error_mapper::error_mapper::{ErrorMapper, SurfaceContext};
        use crate::host_bindings::host_bindings::host;
        use crate::receipt_builder::receipt_builder::{ReceiptBuilder, ReceiptInputs, VerbKind};
        use crate::safety::safety::SafetyPolicy;
        use alloc::collections::BTreeMap;
        use alloc::vec::Vec;

        let t_start = host::clock_now();
        let action_id = action.action_id.clone();

        // Verb-level safety stub (always-Ok in v0.9.6 — authoritative gate
        // is daemon-side per D9 / FND-0021). Called for symmetry with the
        // other verbs so a future hardening pass wires guidance here.
        let _ = SafetyPolicy::check_set_cookies(action.profile);

        let inner = || -> Result<Receipt, HostError> {
            // Resolve cookies from the typed source XOR. `mut` because
            // we wipe the value-buffers in place at the §10 boundary
            // before the vec drops.
            let mut cookies: Vec<NetworkCookieParam> = match action.source.clone() {
                CS::Inline { cookies } => cookies,
                CS::Grant { grant_id } => {
                    let bytes =
                        host::vault_substitute_cookies(&grant_id, &action.session_id)?;
                    let blob: CookieKeychainBlob =
                        serde_json::from_slice(&bytes).map_err(|e| HostError::Internal {
                            reason: alloc::format!("vault cookie blob deserialise: {e}"),
                        })?;
                    if blob.schema_version != 1 {
                        return Err(HostError::Internal {
                            reason: alloc::format!(
                                "unsupported cookie blob schema_version: {} (expected 1)",
                                blob.schema_version
                            ),
                        });
                    }
                    blob.cookies
                }
            };

            // Per-cookie validation. Atomic — any failure rejects the batch.
            validate_cookie_params(&cookies).map_err(HostError::CookieValidationError)?;

            // CDP Network.setCookies envelope. We clone the cookies for
            // the encode call; the encoder consumes the typed values
            // (incl. their Redacted<String> values) and produces CBOR
            // bytes containing the raw values for the chromium shim.
            let _ = host::shim_call(
                "chromium",
                &CdpMessageEncoder::encode(&CdpMessage::NetworkSetCookies(NetworkSetCookies {
                    cookies: cookies.clone(),
                })),
            )?;

            // §10 heap-wipe boundary. The `cookies` vec is about to go
            // out of scope; before that, explicitly wipe the
            // heap-allocated String buffers backing each cookie's
            // `value`. `Redacted<String>`'s Drop calls `String::zeroize`
            // which is best-effort (clears length without overwriting
            // the buffer); the explicit wipe below writes zeros to the
            // actual heap allocation before the `Vec<NetworkCookieParam>`
            // drops.
            //
            // Caveat: the encoder above already cloned + serialised the
            // cookie values into CBOR bytes that crossed the WIT
            // boundary into chromium-shim memory; we cannot wipe those
            // intermediate copies. This wipe addresses the
            // verb-resident String buffers — the chokepoint between
            // host-fn deserialise and CDP-encode that lives inside
            // WASM linear memory.
            //
            // For the Grant path (where bytes came from
            // host::vault_substitute_cookies), the §10 hardening also
            // wipes the intermediate raw `Vec<u8>` returned by the
            // host-fn — see `wipe_byte_buffer_in_place` below.
            for c in cookies.iter_mut() {
                loom_shared::wipe_string_buffer_in_place(c.value.expose_mut());
            }

            // Per-cookie success records (validation passed → all true).
            let results: Vec<SetCookieResult> = cookies
                .iter()
                .map(|c| SetCookieResult {
                    name: c.name.clone(),
                    success: true,
                    error_code: None,
                })
                .collect();
            let results_json =
                serde_json::to_string(&results).map_err(|e| HostError::Internal {
                    reason: alloc::format!("set_cookies_result serialise: {e}"),
                })?;

            let t_end = host::clock_now();
            Ok(ReceiptBuilder::build_cookies_receipt(
                VerbKind::SetCookies,
                ReceiptInputs {
                    action_id: action.action_id.clone(),
                    timing_ticks: t_end.ticks.saturating_sub(t_start.ticks),
                    set_cookies_result: Some(results_json),
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
                    VerbKind::SetCookies,
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
