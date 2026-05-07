// navigate_outcome — types shared between loom-shims (producer) and
// loom-host (consumer) for the PageNavigate receipt data path.
//
// Rationale: `LoomNetworkEvent` and `NavigateOutcome` must be visible to
// both crates. loom-shims → loom-shared is already a dependency; loom-host
// → loom-shared too. loom-host → loom-shims is FORBIDDEN (chromiumoxide
// must not link into the host binary).
//
// Field names and serde tag of `NavigateOutcome` are intentionally a
// strict subset of `loom-shims::ActionResult::Navigated` so that the host
// can CBOR-deserialize the shim's response without importing the shim crate.
// Unknown fields (target_id, frame_id, loader_id, kind) are silently ignored
// by serde.

use serde::{Deserialize, Serialize};

/// Network event captured by the shim during page navigation.
/// Canonical definition; re-exported from loom-shims for backward compat.
/// All numeric fields are integers per Hard Binding 3.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoomNetworkEvent {
    pub method: String,
    pub url: String,
    /// SHA-256 of the canonical request descriptor (Content-Encoding STRIPPED). Hex.
    pub request_hash: String,
    /// SHA-256 of the DECOMPRESSED response body. Hex. Empty iff error_reason.is_some().
    pub response_hash: String,
    pub status: u16,
    pub content_type: String,
    pub duration_ms: u64,
    pub response_bytes: u64,
    pub error_reason: Option<String>,
    /// Shim-side classification of `error_reason`. One of `"dns_failure"`,
    /// `"connect_refused"`, `"tls_error"`, `"network_error"`. `None` for
    /// successful events. Serde-default-`None` keeps the CBOR wire
    /// backward-compatible — pre-existing payloads decode unchanged.
    /// Mirror of the field on `loom_shims::network_interceptor::network_interceptor::LoomNetworkEvent`;
    /// the two definitions stay structurally identical for CBOR round-trip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
}

/// Console line captured by the shim (currently always empty; real capture is followup work).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShimConsoleLine {
    pub level: String,
    pub message: String,
}

/// Sub-resource request that the shim's `NetworkInterceptor` blocked
/// against the default blocklist. Surfaced through `NavigateOutcome`
/// so the host can write a typed `AuditEntry { kind: BlockedUrl, ... }`
/// into the manifest hash chain.
///
/// `reason` is the lowercased section header from `default_blocklist.txt`
/// (e.g. `"analytics"`, `"advertising / ad networks"`); `matched_pattern`
/// is the literal pattern that matched (e.g. `"*.google-analytics.com"`).
/// Both fields are populated by `url_in_blocklist_strict` in
/// `loom_shims::network_interceptor`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlockedEvent {
    pub url: String,
    pub reason: String,
    pub matched_pattern: String,
}

/// Decoded result of a `ShimRequest::PageNavigate` call. Produced by
/// `ShimManager::send_navigate` in loom-host by CBOR-deserializing the
/// `ShimResponse::Ok { payload }` field from the shim.
///
/// Field names match the `ActionResult::Navigated` variant in loom-shims so
/// that CBOR round-trip deserialization works without importing loom-shims.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NavigateOutcome {
    /// The URL originally requested (from `PageNavigate.url`).
    pub url: String,
    /// The final URL after redirects. Currently a stub: same as `url`.
    pub final_url: String,
    /// Page title. Currently a stub: empty string.
    pub page_title: String,
    /// HTTP status code of the main document.
    /// Falls back to 0 when `network_events` is empty.
    pub status_code: u16,
    /// Raw bytes of the DOM snapshot (CBOR-encoded DOM.getDocument response).
    pub dom_bytes: Vec<u8>,
    /// Raw bytes of the screenshot (CBOR-encoded Page.captureScreenshot response).
    pub screenshot_bytes: Vec<u8>,
    /// Network events captured by the shim's NetworkInterceptor.
    pub network_events: Vec<LoomNetworkEvent>,
    /// Console lines captured by the shim. Currently always empty.
    pub console_lines: Vec<ShimConsoleLine>,
    /// Sub-resource requests blocked by the default blocklist.
    /// Each entry becomes a manifest
    /// `AuditEntry { kind: BlockedUrl }` on the host side. `serde(default)`
    /// keeps the CBOR wire backward-compatible — pre-feature payloads
    /// decode unchanged with an empty vec.
    #[serde(default)]
    pub blocked_events: Vec<BlockedEvent>,
    /// SHA-256 hex of `dom_bytes` (precomputed by shim; host verifies via ContentStore.put).
    pub dom_after_sha256: String,
    /// SHA-256 hex of `screenshot_bytes` (precomputed by shim).
    pub screenshot_sha256: String,
}
