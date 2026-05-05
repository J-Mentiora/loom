// receipt_builder — tiered receipt types + capture-profile transformation.
//
// # Contract semantics
// - **Flat struct (design decision Q2).** One `ReceiptPayload` struct with
//   `Option` fields. Tier rules determine which are populated. serde_jcs
//   produces lexicographically ordered JSON regardless of field presence.
// - **Uses content_store::ContentRef (A1 fix).** No duplicate type.
// - **Canonical-JSON via serde_jcs.**
//   `canonical_bytes()` is the single point of serialization.
// - **No floats.** All numeric fields are `u64` or `u32`.
// - **Message truncation at 280 chars (S1 fix).**
//   `build_error_receipt` enforces at construction time.
// - **`ReceiptStatus` enum (S3 fix).** `Ok | Error` with lowercase serde wire.

use crate::content_store::ContentRef;
use crate::error::LoomError;
use crate::error_types::{ReceiptCode, ReceiptSurface};
use serde::{Deserialize, Serialize};

/// How much to capture per session.
///
/// - `Default` — tier-assigned fields only (hash for click, blob for navigate, etc.)
/// - `Full` — adds `dom_before_blob_ref` + `screenshot_before_blob_ref` to every action
/// - `Minimal` — hash-only; no blob refs; no network event bodies; no console lines
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureProfile {
    Default,
    Full,
    Minimal,
}

/// Parse the wire form of `capture_policy` into a
/// `CaptureProfile`. Wire form is the lowercased variant; unknown
/// values yield `None` (server-side validation in
/// `session_validation` rejects unknowns before reaching this layer).
pub fn capture_profile_from_str(s: &str) -> Option<CaptureProfile> {
    match s {
        "minimal" => Some(CaptureProfile::Minimal),
        "default" => Some(CaptureProfile::Default),
        "full" => Some(CaptureProfile::Full),
        _ => None,
    }
}

/// Receipt status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReceiptStatus {
    Ok,
    Error,
}

/// Network event recorded during an action (navigate tier).
/// All numeric fields are integers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkEvent {
    pub method: String,
    pub url: String,
    pub status_code: u32,
    /// SHA-256 hex of the response body (64 lowercase hex chars). Always present.
    pub response_body_sha256_hex: String,
    pub response_body_size_bytes: u64,
    /// Full blob ref — None in Minimal capture.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_body_ref: Option<ContentRef>,
    pub timing_ticks: u64,
    /// Content-type (MIME) of the response body. Sourced from
    /// `LoomNetworkEvent.content_type` (shim-side). Empty string when
    /// unknown. `serde(default)` keeps pre-feature receipts
    /// deserializable; `skip_serializing_if = "String::is_empty"` keeps
    /// canonical-JSON shape stable for non-navigate receipts whose
    /// `network_events` is empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content_type: String,
}

/// Console output line (navigate + evaluate tiers).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleLine {
    pub level: String,
    pub message: String,
    pub timing_ticks: u64,
}

/// Aggregate network summary on the navigate-tier wire receipt.
/// Per-request detail lives in
/// `network_events` (manifest) and `side_effects[]` (wire). All numeric
/// fields are integers.
///
/// **`total_bytes` is response-body bytes only** (sum of
/// `LoomNetworkEvent.response_bytes`). The shim doesn't currently
/// surface request-body bytes, so a navigate that uploads a 10MB POST
/// body will report a small `total_bytes` (just the response body).
/// Add a `request_bytes` field if/when the shim plumbs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkSummary {
    pub total_count: u64,
    /// Sum of `LoomNetworkEvent.response_bytes` across all events —
    /// **response bodies only**, not request bodies.
    pub total_bytes: u64,
    /// Count of events where `status >= 400` OR `error_reason.is_some()`.
    pub error_count: u64,
}

/// Typed receipt payload. All fields populated by builder methods;
/// `apply_capture_profile` post-processes per session capture setting.
///
/// All numeric fields are integers.
/// Field keys serialize in lexicographic order via `serde_jcs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptPayload {
    pub action_id: String,
    /// Receipt status/error discriminator.
    pub code: ReceiptCode,
    // ---- Error receipt fields ----
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    // ---- Hash-only after-fields — click default tier ----
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dom_after_hash: Option<String>,
    // ---- Full-blob after-fields — navigate default tier ----
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dom_after_blob_ref: Option<ContentRef>,
    // ---- Full-blob before-fields — full capture override ----
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dom_before_blob_ref: Option<ContentRef>,
    // ---- Error receipt message (≤ 280 chars) ----
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    // ---- Navigate arrays ----
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub network_events: Vec<NetworkEvent>,
    // ---- Evaluate return value ----
    // Canonical-JSON of evaluated value. None when value > 64KB
    // (offloaded to content store; see return_value_blob_ref).
    // Truncation discriminator = return_value_blob_ref.is_some().
    // serde alias preserves backward-compat with on-disk receipts that
    // used the pre-rename `return_value` field.
    #[serde(skip_serializing_if = "Option::is_none", default, alias = "return_value")]
    pub return_value_json: Option<String>,
    /// ContentRef when canonical-JSON bytes > 64KB.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub return_value_blob_ref: Option<ContentRef>,
    // ---- Screenshot hash-only — click default tier ----
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_after_hash: Option<String>,
    // ---- Screenshot full-blob — navigate default tier ----
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_after_blob_ref: Option<ContentRef>,
    // ---- Screenshot before-blob — full capture override ----
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_before_blob_ref: Option<ContentRef>,
    // ---- LLM cache hit flag ----
    /// True when an LLM call was served from the session LLM cache;
    /// None for non-LLM actions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_cache_hit: Option<bool>,
    /// Receipt status: Ok or Error.
    pub status: ReceiptStatus,
    /// Surface that emitted this receipt.
    pub surface: ReceiptSurface,
    /// Monotonically non-decreasing microsecond timestamp.
    pub timing_ticks: u64,
    // ---- Console lines — navigate + evaluate tiers ----
    // Note: field name `console_lines` comes lexicographically between `code` and `details`,
    // but since serde_jcs sorts alphabetically it will appear in correct position.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub console_lines: Vec<ConsoleLine>,
    // ---- Navigate tier-2 fields ----
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u32>,
    /// SHA-256 hex of the DOM snapshot blob in ContentStore.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dom_snapshot_hash: Option<String>,
    /// Stub: always 0 (console capture wired in a later release).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub console_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_count: Option<u64>,
    /// Session-monotonic ms timestamp from DeterminismHarness.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emitted_at_ms: Option<u64>,
}

impl ReceiptPayload {
    /// Canonical-JSON bytes for manifest storage. Uses serde_jcs (RFC 8785).
    /// Field keys are in lexicographic order. SHA-256 of output is stable across
    /// runs and machines.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, LoomError> {
        serde_jcs::to_string(self)
            .map(|s| s.into_bytes())
            .map_err(|e| {
                LoomError::internal(format!("receipt canonical serialization failed: {e}"))
            })
    }
}

/// Stateless builder — one method per action tier + error builder.
pub struct ReceiptBuilder;

impl ReceiptBuilder {
    /// Click default tier: hash-only fields; code = WebActionCompleted.
    pub fn build_click_receipt(
        action_id: String,
        timing_ticks: u64,
        dom_after_hash: String,
        screenshot_after_hash: String,
    ) -> ReceiptPayload {
        ReceiptPayload {
            action_id,
            code: ReceiptCode::WebActionCompleted,
            details: None,
            dom_after_hash: Some(dom_after_hash),
            dom_after_blob_ref: None,
            dom_before_blob_ref: None,
            message: None,
            network_events: Vec::new(),
            return_value_json: None,
            return_value_blob_ref: None,
            screenshot_after_hash: Some(screenshot_after_hash),
            screenshot_after_blob_ref: None,
            screenshot_before_blob_ref: None,
            llm_cache_hit: None,
            status: ReceiptStatus::Ok,
            surface: ReceiptSurface::Web,
            timing_ticks,
            console_lines: Vec::new(),
            url: None,
            final_url: None,
            title: None,
            status_code: None,
            dom_snapshot_hash: None,
            console_count: None,
            network_count: None,
            emitted_at_ms: None,
        }
    }

    /// Navigate default tier: full blob refs + network events + console.
    pub fn build_navigate_receipt(
        action_id: String,
        timing_ticks: u64,
        dom_after_blob_ref: ContentRef,
        screenshot_after_blob_ref: ContentRef,
        network_events: Vec<NetworkEvent>,
        console_lines: Vec<ConsoleLine>,
    ) -> ReceiptPayload {
        ReceiptPayload {
            action_id,
            code: ReceiptCode::WebActionCompleted,
            details: None,
            dom_after_hash: None,
            dom_after_blob_ref: Some(dom_after_blob_ref),
            dom_before_blob_ref: None,
            message: None,
            network_events,
            return_value_json: None,
            return_value_blob_ref: None,
            screenshot_after_hash: None,
            screenshot_after_blob_ref: Some(screenshot_after_blob_ref),
            screenshot_before_blob_ref: None,
            llm_cache_hit: None,
            status: ReceiptStatus::Ok,
            surface: ReceiptSurface::Web,
            timing_ticks,
            console_lines,
            url: None,
            final_url: None,
            title: None,
            status_code: None,
            dom_snapshot_hash: None,
            console_count: None,
            network_count: None,
            emitted_at_ms: None,
        }
    }

    /// Evaluate default tier.
    /// Exactly one of `return_value_json` / `return_value_blob_ref` is Some
    /// on success; truncation discriminator = `return_value_blob_ref.is_some()`.
    pub fn build_evaluate_receipt(
        action_id: String,
        timing_ticks: u64,
        return_value_json: Option<String>,
        return_value_blob_ref: Option<ContentRef>,
        console_lines: Vec<ConsoleLine>,
    ) -> ReceiptPayload {
        ReceiptPayload {
            action_id,
            code: ReceiptCode::WebActionCompleted,
            details: None,
            dom_after_hash: None,
            dom_after_blob_ref: None,
            dom_before_blob_ref: None,
            message: None,
            network_events: Vec::new(),
            return_value_json,
            return_value_blob_ref,
            screenshot_after_hash: None,
            screenshot_after_blob_ref: None,
            screenshot_before_blob_ref: None,
            llm_cache_hit: None,
            status: ReceiptStatus::Ok,
            surface: ReceiptSurface::Web,
            timing_ticks,
            console_lines,
            url: None,
            final_url: None,
            title: None,
            status_code: None,
            dom_snapshot_hash: None,
            console_count: None,
            network_count: None,
            emitted_at_ms: None,
        }
    }

    /// Error receipt: typed code + message (≤ 280 chars) + surface.
    pub fn build_error_receipt(
        action_id: String,
        timing_ticks: u64,
        code: ReceiptCode,
        message: String,
        surface: ReceiptSurface,
        details: Option<serde_json::Value>,
    ) -> ReceiptPayload {
        // S1 fix: enforce ≤ 280 char message
        let message = if message.len() > 280 {
            message.chars().take(280).collect()
        } else {
            message
        };
        ReceiptPayload {
            action_id,
            code,
            details,
            dom_after_hash: None,
            dom_after_blob_ref: None,
            dom_before_blob_ref: None,
            message: Some(message),
            network_events: Vec::new(),
            return_value_json: None,
            return_value_blob_ref: None,
            screenshot_after_hash: None,
            screenshot_after_blob_ref: None,
            screenshot_before_blob_ref: None,
            llm_cache_hit: None,
            status: ReceiptStatus::Error,
            surface,
            timing_ticks,
            console_lines: Vec::new(),
            url: None,
            final_url: None,
            title: None,
            status_code: None,
            dom_snapshot_hash: None,
            console_count: None,
            network_count: None,
            emitted_at_ms: None,
        }
    }

    /// LLM call receipt: records LLM response body + cache-hit flag.
    pub fn build_llm_receipt(
        action_id: String,
        timing_ticks: u64,
        response_body: serde_json::Value,
        cache_hit: bool,
    ) -> ReceiptPayload {
        ReceiptPayload {
            action_id,
            code: ReceiptCode::WebActionCompleted,
            details: Some(response_body),
            dom_after_hash: None,
            dom_after_blob_ref: None,
            dom_before_blob_ref: None,
            message: None,
            network_events: Vec::new(),
            return_value_json: None,
            return_value_blob_ref: None,
            screenshot_after_hash: None,
            screenshot_after_blob_ref: None,
            screenshot_before_blob_ref: None,
            llm_cache_hit: Some(cache_hit),
            status: ReceiptStatus::Ok,
            surface: ReceiptSurface::Web,
            timing_ticks,
            console_lines: Vec::new(),
            url: None,
            final_url: None,
            title: None,
            status_code: None,
            dom_snapshot_hash: None,
            console_count: None,
            network_count: None,
            emitted_at_ms: None,
        }
    }
}
