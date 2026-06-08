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

/// One observed network request (raw, per-hop), captured by the shim's
/// full-capture network-entries path. This is the OBSERVATIONAL side-channel
/// data behind the navigate receipt's `network_entries` and the
/// `loom.web.network_log` tool — it is NEVER part of the replay hash chain
/// (see `loom-core::receipt_builder::ReceiptPayload`, which deliberately
/// omits it). Distinct from `LoomNetworkEvent` (the Document-only, hashed
/// path that derives `status_code`).
///
/// Metadata only — never request/response bodies or headers. `status == 0`
/// means "no final response observed" (request still pending at snapshot OR
/// `loadingFailed`). Redirect hops share `request_id` (one entry per hop).
/// `ts_ms` is wall-clock epoch milliseconds (CDP `requestWillBeSent.wallTime`)
/// — permissible here precisely because this data is excluded from the
/// deterministic hash chain. All numeric fields are integers.
///
/// Mirror of `loom_shims::network_interceptor::network_interceptor::LoomNetworkEntry`;
/// the two definitions stay structurally identical for CBOR round-trip.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoomNetworkEntry {
    pub url: String,
    pub method: String,
    pub status: u16,
    pub resource_type: String,
    pub from_cache: bool,
    pub request_id: String,
    pub ts_ms: u64,
}

/// Decoded result of a `ShimRequest::GetNetworkLog` call (the
/// `loom.web.network_log` tool). Carries the session-accumulated network
/// entries observed since the last navigate, plus the shim-side truncation
/// flag. `serde(default)` on both for CBOR wire back-compat.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct NetworkLogOutcome {
    #[serde(default)]
    pub network_entries: Vec<LoomNetworkEntry>,
    #[serde(default)]
    pub network_entries_truncated: bool,
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
    /// Raw per-request network entries observed during this navigate (the
    /// full-capture, non-hashed side-channel — xhr/fetch/subresource +
    /// document, NOT just the main document). `serde(default)` keeps the CBOR
    /// wire backward-compatible — pre-feature payloads decode with an empty vec.
    #[serde(default)]
    pub network_entries: Vec<LoomNetworkEntry>,
    /// True when the session accumulator hit its `max_network_entries` cap and
    /// dropped further entries (the returned list is the first-N). `serde(default)`
    /// for wire back-compat.
    #[serde(default)]
    pub network_entries_truncated: bool,
    /// settle-capture: the readiness mode the capture was gated on
    /// (`load|networkidle|settled`). `serde(default)` (→ `settled`) keeps
    /// pre-feature CBOR payloads decoding unchanged.
    #[serde(default = "default_settle_until_outcome")]
    pub settle_until: String,
    /// How the readiness wait ended: `reached|timeout|dom_unstable`.
    #[serde(default = "default_settle_outcome")]
    pub settle_outcome: String,
    /// Virtual-tick count translated to ms by the shim's session clock. A
    /// diagnostic ONLY — excluded from the host's `outcome_hash` (like the
    /// screenshot hash) so it never affects replay byte-equality.
    #[serde(default)]
    pub settle_ms: u64,
    /// In-flight (non-WebSocket/EventSource) request count observed at settle.
    #[serde(default)]
    pub network_count_at_settle: u64,
}

fn default_settle_until_outcome() -> String {
    "settled".to_string()
}

fn default_settle_outcome() -> String {
    "reached".to_string()
}

/// settle-capture slice 2: decoded result of a `ShimRequest::WaitFor` call.
/// Produced by `ShimManager::send_wait_for` by CBOR-deserializing the shim's
/// `ActionResult::Waited` payload (field names match that variant; the `kind`
/// tag and any unknown fields are ignored by serde). `emitted_at_ms` is stamped
/// host-side from the DeterminismHarness, like the navigate path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitOutcome {
    /// Readiness mode that was waited for (`load|networkidle|settled`).
    #[serde(default = "default_settle_until_outcome")]
    pub settle_until: String,
    /// How the readiness wait ended (`reached|timeout|dom_unstable`).
    #[serde(default = "default_settle_outcome")]
    pub settle_outcome: String,
    /// Virtual-tick count translated to ms. Diagnostic only — excluded from the
    /// host's `outcome_hash` so it never affects replay byte-equality.
    #[serde(default)]
    pub settle_ms: u64,
    /// In-flight (non-WebSocket/EventSource) request count observed at settle.
    #[serde(default)]
    pub network_count_at_settle: u64,
}
