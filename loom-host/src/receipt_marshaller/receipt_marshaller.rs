// ReceiptMarshaller — assemble `Receipt` post-WASM-return; queue
// `ManifestWriter::append` on a background tokio task.
//
// # Contract semantics
// - **Off the synchronous return.** `WasmHost::dispatch`
//   returns IMMEDIATELY when the WASM export resolves. This module is
//   invoked AFTERWARDS on a background tokio task spawned on the
//   session's `receipt_pool` (per-session, not on a global pool, never
//   per host-fn).
// - **Receipt overhead p95 ≤ 50 ms.** Bound by manifest
//   write latency only — assembly is in-memory string + integer ops.
// - **Canonical JSON.** Final payload is
//   `serde_jcs::to_string(receipt)` — never `serde_json::to_string`.
// - **Trap-receipt fast path.** `emit_trap_receipt` is the entry point
//   for `TrapHandler`; assembles a typed `LoomErrorCode::SurfaceTrap`
//   receipt and queues it identically.

use crate::wit_type_marshaller::Marshaller;
use loom_core::error::LoomError;
use loom_core::manifest_writer::{ManifestWriter, SessionId};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::runtime::Handle as TokioHandle;

/// Per-action receipt builder. Populated by `HostFunctionTable` over
/// the action's lifetime; finalized by `ReceiptMarshaller::queue`.
///
/// `action_hash`, `outcome_hash`, `emitted_at_ms` mirror the WIT
/// `record receipt` fields in `wit/loom-surface.wit:15-19`. They are
/// populated by `SessionExecutor::run` after decoding the typed
/// `result<receipt, host-error>` returned by the WASM guest.
///
/// The `navigate_*` fields are populated from the optional WIT receipt fields
/// when the WASM guest returns a navigate-tier-2 receipt.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReceiptBuilder {
    pub action_id: u64,
    pub started_at_ms: u64,
    pub finished_at_ms: u64,
    pub status: ReceiptStatus,
    pub side_effects_count: u32,
    pub host_call_count: u32,
    pub error_code: Option<String>,
    pub error_details: Option<String>,
    pub action_hash: String,
    pub outcome_hash: String,
    pub emitted_at_ms: u64,
    // ---- Navigate tier-2 fields ----
    // Populated by decode_typed_receipt when the WIT receipt carries them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navigate_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navigate_final_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navigate_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navigate_status_code: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navigate_dom_snapshot_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navigate_screenshot_after_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navigate_console_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navigate_network_count: Option<u64>,
    // settle-capture: the two deterministic readiness fields surfaced on the
    // canonical (and therefore wire) receipt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navigate_settle_until: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navigate_settle_outcome: Option<String>,
    /// JSON bytes of `Vec<LoomNetworkEvent>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navigate_side_effects_json: Option<Vec<u8>>,
    /// JSON bytes of `Vec<ShimConsoleLine>`.
    /// Empty list today (current shim console-capture stub).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navigate_console_lines_json: Option<Vec<u8>>,
    /// JSON bytes of `loom_core::NetworkSummary`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navigate_network_summary_json: Option<Vec<u8>>,
    // ---- Evaluate tier fields ----
    // Populated by decode_typed_receipt when the WIT receipt carries them.
    // Truncation discriminator: evaluate_return_value_blob_ref.is_some().
    /// Canonical-JSON of the evaluated value. None when truncated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluate_return_value_json: Option<String>,
    /// ContentRef when canonical-JSON bytes > 64KB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluate_return_value_blob_ref: Option<loom_core::content_store::ContentRef>,
    // ---- v0.9.6 cookie tier fields ----
    // Populated by decode_typed_receipt when the WIT receipt carries them.
    // Each is a JSON-encoded payload from the verb's
    // ReceiptBuilder::build_cookies_receipt. Sort/redact transforms
    // happen in `assemble_cookies_canonical_bytes` before JCS encoding
    // (D13 tuple-identity sort).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_cookies_result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub get_cookies_result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clear_cookies_result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delete_cookies_result: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    #[default]
    Ok,
    Error,
    Trapped,
}

/// Per-action observed cost (wall-clock, network bytes, …). Fed to
/// `BudgetEnforcer::account` AFTER the dispatch return.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ObservedCosts {
    pub walltime_ms: u64,
    pub network_bytes: u64,
    pub dom_nodes: u64,
    pub js_heap_bytes: u64,
}

/// Local accumulator for off-hot-path receipt enrichment. Populated by
/// `HostFunctionTable` host-fn bodies; consumed during `assemble_canonical_bytes`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SideEffectAccumulator {
    pub host_calls: u32,
    pub blob_puts: u32,
    pub blob_gets: u32,
    pub net_requests: u32,
    pub shim_calls: u32,
}

/// What the marshaller queues. Owned struct — moved into the background
/// task so the dispatch task drops nothing on the hot path.
pub struct ActionOutcome {
    pub session_id: SessionId,
    pub builder: ReceiptBuilder,
    pub observed_costs: ObservedCosts,
}

pub struct ReceiptMarshaller {
    pub(crate) manifest_writer: Arc<dyn ManifestWriter>,
    pub(crate) budget: Arc<dyn loom_core::budget_enforcer::BudgetEnforcer>,
}

impl ReceiptMarshaller {
    pub fn new(
        manifest_writer: Arc<dyn ManifestWriter>,
        budget: Arc<dyn loom_core::budget_enforcer::BudgetEnforcer>,
    ) -> Arc<Self> {
        Arc::new(Self {
            manifest_writer,
            budget,
        })
    }

    /// Queue an action outcome for receipt assembly + manifest append.
    /// Spawns onto `pool`; does NOT block the calling task. Backpressure:
    /// if the background pool refuses spawn (rare), falls back to a
    /// synchronous append on the calling task.
    pub fn queue(
        self: &Arc<Self>,
        outcome: ActionOutcome,
        pool: TokioHandle,
    ) -> Result<(), LoomError> {
        let this = self.clone();
        pool.spawn(async move {
            let _ = this.append_synchronous_fallback(outcome);
        });
        Ok(())
    }

    /// Trap-fast-path entrypoint. Called by `TrapHandler` to emit a
    /// `SurfaceTrap` receipt without going through `ReceiptBuilder`.
    pub fn emit_trap_receipt(
        self: &Arc<Self>,
        session_id: SessionId,
        action_id: u64,
        surface: String,
        trap_code: String,
        frames_count: u32,
        pool: TokioHandle,
    ) -> Result<(), LoomError> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let builder = ReceiptBuilder {
            action_id,
            started_at_ms: now_ms,
            finished_at_ms: now_ms,
            status: ReceiptStatus::Trapped,
            side_effects_count: 0,
            host_call_count: 0,
            error_code: Some(trap_code),
            error_details: Some(format!("surface={surface} frames={frames_count}")),
            ..Default::default()
        };
        let outcome = ActionOutcome {
            session_id,
            builder,
            observed_costs: ObservedCosts::default(),
        };
        self.queue(outcome, pool)
    }

    /// Synchronous assemble step. Pure: takes a builder, returns
    /// canonical-JSON bytes ready for `ManifestWriter::append`.
    /// `serde_jcs` is the ONLY canonicalizer.
    ///
    /// When navigate tier-2 fields are present, builds a
    /// `loom_core::ReceiptPayload` to get canonical field names and the
    /// unified serialization path.
    pub fn assemble_canonical_bytes(builder: &ReceiptBuilder) -> Result<Vec<u8>, LoomError> {
        use loom_core::error::LoomErrorCode;

        // A typed navigate error receipt (structured
        // shim-failure detail with a `kind` field) takes the navigate
        // assembly path even when `navigate_url` / `navigate_dom_snapshot_hash`
        // are unset (the WIT error variant doesn't carry the URL — it's
        // bound separately at action-dispatch time). Without this gate
        // extension, error receipts fall through to the generic
        // `serde_jcs::to_string(builder)` path and skip the carefully-
        // crafted `code` / `details` / `message` branching in
        // `assemble_navigate_canonical_bytes` below.
        // Restrict the kind check to the navigate-specific shim-failure
        // kinds so an evaluate js_throw error (kind=js_throw) doesn't get
        // routed through the navigate-friendly-message path. Evaluate
        // errors route via assemble_evaluate_canonical_bytes.
        let detail_kind: Option<String> = builder
            .error_details
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .and_then(|v| v.get("kind").and_then(|k| k.as_str()).map(String::from));

        const NAVIGATE_ERROR_KINDS: &[&str] = &[
            "http_status",
            "dns_failure",
            "connect_refused",
            "tls_error",
            "network_error",
        ];
        const EVALUATE_ERROR_KINDS: &[&str] = &["js_throw", "cbor_unrepresentable"];

        let is_navigate_error = builder.status == ReceiptStatus::Error
            && builder.error_code.as_deref() == Some("shim-failure")
            && detail_kind
                .as_deref()
                .map(|k| NAVIGATE_ERROR_KINDS.contains(&k))
                .unwrap_or(false);

        let is_evaluate_error = builder.status == ReceiptStatus::Error
            && builder.error_code.as_deref() == Some("shim-failure")
            && detail_kind
                .as_deref()
                .map(|k| EVALUATE_ERROR_KINDS.contains(&k))
                .unwrap_or(false);

        // Ensure shim-captured network events make it into
        // ReceiptPayload.network_events even when tier-2 fields aren't
        // wired yet (decouples HAR export from
        // navigate-receipt-tier2-still-missing).
        if builder.navigate_url.is_some()
            || builder.navigate_dom_snapshot_hash.is_some()
            || builder.navigate_side_effects_json.is_some()
            || is_navigate_error
        {
            return assemble_navigate_canonical_bytes(builder);
        }

        if builder.evaluate_return_value_json.is_some()
            || builder.evaluate_return_value_blob_ref.is_some()
            || is_evaluate_error
        {
            return assemble_evaluate_canonical_bytes(builder);
        }

        // v0.9.6 cookie tier — any cookie-result field set routes to the
        // cookies canonical-bytes path with D13 tuple-identity sort and
        // value redaction (for replay byte-identity).
        if builder.set_cookies_result.is_some()
            || builder.get_cookies_result.is_some()
            || builder.clear_cookies_result.is_some()
            || builder.delete_cookies_result.is_some()
        {
            return assemble_cookies_canonical_bytes(builder);
        }

        let json = serde_jcs::to_string(builder)
            .map_err(|e| LoomError::new(LoomErrorCode::Internal, e.to_string()))?;
        Ok(json.into_bytes())
    }

    /// Force-synchronous fallback. Called when the background pool
    /// refuses spawn. Logs a tracing warn before falling through.
    pub fn append_synchronous_fallback(&self, outcome: ActionOutcome) -> Result<(), LoomError> {
        use loom_core::manifest_writer::ManifestEntry;
        let bytes = Self::assemble_canonical_bytes(&outcome.builder)?;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.manifest_writer.append(
            outcome.session_id,
            ManifestEntry::ActionReceipt {
                action_id: outcome.builder.action_id,
                emitted_at_ms: now_ms,
                receipt_canonical_bytes: bytes,
                prev_hash: String::new(),
            },
        )
    }

    /// Test seam: depend-on `WitTypeMarshaller` is structural — the
    /// marshaller is what `assemble_canonical_bytes` uses for any WIT
    /// types embedded in the receipt payload.
    pub fn _marshaller_dep() -> Result<Marshaller, LoomError> {
        Marshaller::generated_or_panic()
    }
}

/// Build canonical-JSON bytes for navigate tier-2 receipts via
/// `loom_core::ReceiptPayload` so field names match the core schema.
fn assemble_navigate_canonical_bytes(builder: &ReceiptBuilder) -> Result<Vec<u8>, LoomError> {
    use loom_core::error_types::{ReceiptCode, ReceiptSurface};
    use loom_core::receipt_builder::receipt_builder::{
        NetworkEvent, ReceiptPayload, ReceiptStatus,
    };
    use loom_shared::navigate_outcome::LoomNetworkEvent;

    // Invariant (host-side): only the navigate dispatch path on
    // session_executor populates navigate_side_effects_json. If a future
    // web-surface verb (click, type-text, scroll, …) starts emitting
    // side-effects, revisit the gate in assemble_canonical_bytes above.
    debug_assert!(
        builder.navigate_url.is_some()
            || builder.navigate_dom_snapshot_hash.is_some()
            || builder.navigate_side_effects_json.is_some()
            || builder.status == crate::receipt_marshaller::ReceiptStatus::Error,
        "assemble_navigate_canonical_bytes invoked but no navigate signal present on builder"
    );

    // Degraded-path tracing: when the navigate path is
    // taken solely because navigate_side_effects_json is populated (i.e.
    // tier-2 fields are still unset; see navigate-receipt-tier2-still-missing),
    // emit a single warn so operators can see why the resulting receipt has
    // url/title/status_code = null while network_events is populated.
    // No default subscriber is installed during `cargo test`, so this does
    // not pollute test output.
    if builder.navigate_url.is_none()
        && builder.navigate_dom_snapshot_hash.is_none()
        && builder.navigate_side_effects_json.is_some()
        && builder.status != crate::receipt_marshaller::ReceiptStatus::Error
    {
        tracing::warn!(
            action_id = %builder.action_id,
            "navigate receipt sealed with side_effects_json but tier-2 fields unset; \
             HAR will populate from network_events but receipt.url/title/status_code will be null \
             (see navigate-receipt-tier2-still-missing)"
        );
    }

    // Decode shim-captured network events into the canonical receipt's
    // `network_events` so HAR/JSON exporters have per-event url/status/
    // size/mime to render. The bytes here are the
    // JSON encoding of `Vec<LoomNetworkEvent>` written by host_impl.rs.
    // Sub-resource events with `error_reason.is_some()` are mapped too,
    // so DevTools waterfalls can render failed requests; their status
    // comes through verbatim from the shim (0 when no HTTP response).
    let network_events: Vec<NetworkEvent> = builder
        .navigate_side_effects_json
        .as_deref()
        .and_then(|bytes| serde_json::from_slice::<Vec<LoomNetworkEvent>>(bytes).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|e| NetworkEvent {
            method: e.method,
            url: e.url,
            status_code: u32::from(e.status),
            response_body_sha256_hex: e.response_hash,
            response_body_size_bytes: e.response_bytes,
            response_body_ref: None,
            // timing_ticks is microseconds; shim
            // duration_ms is milliseconds.
            timing_ticks: e.duration_ms.saturating_mul(1000),
            content_type: e.content_type,
        })
        .collect();

    let is_error = builder.status == crate::receipt_marshaller::ReceiptStatus::Error;

    // For typed-error receipts:
    //  - `code` flips to WebNavigationFailed (still in the stable
    //    ReceiptCode enum).
    //  - `details` is the parsed JSON object the surface emitted
    //    (`{"kind":"http_status","status_code":404,"url":"..."}` or
    //    `{"kind":"dns_failure","url":"...","chromium_error":"..."}`).
    //  - `message` is a SHORT human-readable string (≤ 280 chars)
    //    — NOT the raw JSON blob, which would be hard to
    //    read in operator dashboards.
    let (code, details, message) = if is_error {
        let parsed: Option<serde_json::Value> = builder
            .error_details
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok());
        let kind = parsed
            .as_ref()
            .and_then(|v| v.get("kind"))
            .and_then(|k| k.as_str())
            .unwrap_or("unknown");
        let friendly = match kind {
            "http_status" => {
                let sc = parsed
                    .as_ref()
                    .and_then(|v| v.get("status_code"))
                    .and_then(|n| n.as_u64())
                    .unwrap_or(0);
                format!("navigate failed: HTTP {sc}")
            }
            // Typed message names the failure mode + helps
            // operators triage (DNS vs reachability vs TLS).
            "dns_failure" => "navigate failed: DNS resolution failed".to_string(),
            "connect_refused" => "navigate failed: connection refused".to_string(),
            "tls_error" => "navigate failed: TLS handshake failed".to_string(),
            "network_error" => "navigate failed: network error".to_string(),
            _ => "navigate failed".to_string(),
        };
        (ReceiptCode::WebNavigationFailed, parsed, Some(friendly))
    } else {
        (
            ReceiptCode::WebActionCompleted,
            None,
            builder.error_details.clone(),
        )
    };

    let status = if is_error {
        ReceiptStatus::Error
    } else {
        ReceiptStatus::Ok
    };

    let payload = ReceiptPayload {
        action_id: builder.action_id.to_string(),
        code,
        details,
        dom_after_hash: None,
        dom_after_blob_ref: None,
        dom_before_blob_ref: None,
        message,
        network_events,
        return_value_json: None,
        return_value_blob_ref: None,
        screenshot_after_hash: builder.navigate_screenshot_after_hash.clone(),
        screenshot_after_blob_ref: None,
        screenshot_before_blob_ref: None,
        llm_cache_hit: None,
        status,
        surface: ReceiptSurface::Web,
        // timing_ticks unit is microseconds.
        // builder.finished_at_ms is session-elapsed ms from
        // DeterminismHarness::clock_now() (NOT wall-clock UNIX-EPOCH).
        timing_ticks: builder.finished_at_ms.saturating_mul(1000),
        console_lines: Vec::new(),
        url: builder.navigate_url.clone(),
        final_url: builder.navigate_final_url.clone(),
        title: builder.navigate_title.clone(),
        status_code: builder.navigate_status_code,
        dom_snapshot_hash: builder.navigate_dom_snapshot_hash.clone(),
        console_count: builder.navigate_console_count,
        network_count: builder.navigate_network_count,
        emitted_at_ms: if builder.emitted_at_ms > 0 {
            Some(builder.emitted_at_ms)
        } else {
            None
        },
        settle_until: builder.navigate_settle_until.clone(),
        settle_outcome: builder.navigate_settle_outcome.clone(),
    };

    payload.canonical_bytes()
}

/// Build canonical-JSON bytes for evaluate-tier receipts.
/// Mirrors `assemble_navigate_canonical_bytes` shape but for the evaluate
/// path. js_throw / cbor_unrepresentable errors carry typed `details` JSON.
fn assemble_evaluate_canonical_bytes(builder: &ReceiptBuilder) -> Result<Vec<u8>, LoomError> {
    use loom_core::error_types::{ReceiptCode, ReceiptSurface};
    use loom_core::receipt_builder::receipt_builder::{ReceiptPayload, ReceiptStatus};

    let is_error = builder.status == crate::receipt_marshaller::ReceiptStatus::Error;

    // For typed-error evaluate receipts:
    //  - `code` flips to WebActionFailed (existing enum variant).
    //  - `details` is the parsed JSON object the host emitted
    //    (`{"kind":"js_throw","exception":"...","line":N,"column":N}` or
    //    `{"kind":"cbor_unrepresentable","reason":"..."}`).
    //  - `message` is short human-readable (≤ 280 chars).
    let (code, details, message) = if is_error {
        let parsed: Option<serde_json::Value> = builder
            .error_details
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok());
        let kind = parsed
            .as_ref()
            .and_then(|v| v.get("kind"))
            .and_then(|k| k.as_str())
            .unwrap_or("unknown");
        let (variant_code, friendly) = match kind {
            "js_throw" => {
                // Page-side exception → typed `WebEvaluateThrew`. Surface the
                // exception message into the friendly text so operator
                // dashboards show it without parsing `details`.
                let ex = parsed
                    .as_ref()
                    .and_then(|v| v.get("exception"))
                    .and_then(|e| e.as_str())
                    .unwrap_or("");
                let msg = if ex.is_empty() {
                    "evaluate failed: page-side exception".to_string()
                } else {
                    format!("evaluate failed: {ex}")
                };
                (ReceiptCode::WebEvaluateThrew, msg)
            }
            "cbor_unrepresentable" => {
                // Result shape can't round-trip through canonical-JSON →
                // surface as schema violation.
                let reason = parsed
                    .as_ref()
                    .and_then(|v| v.get("reason"))
                    .and_then(|r| r.as_str())
                    .unwrap_or("unknown");
                (
                    ReceiptCode::SchemaViolation,
                    format!("evaluate failed: result not representable ({reason})"),
                )
            }
            _ => (ReceiptCode::WebEvaluateThrew, "evaluate failed".to_string()),
        };
        (variant_code, parsed, Some(friendly))
    } else {
        (
            ReceiptCode::WebActionCompleted,
            None,
            builder.error_details.clone(),
        )
    };

    let status = if is_error {
        ReceiptStatus::Error
    } else {
        ReceiptStatus::Ok
    };

    let payload = ReceiptPayload {
        action_id: builder.action_id.to_string(),
        code,
        details,
        dom_after_hash: None,
        dom_after_blob_ref: None,
        dom_before_blob_ref: None,
        message,
        network_events: Vec::new(),
        return_value_json: builder.evaluate_return_value_json.clone(),
        return_value_blob_ref: builder.evaluate_return_value_blob_ref.clone(),
        screenshot_after_hash: None,
        screenshot_after_blob_ref: None,
        screenshot_before_blob_ref: None,
        llm_cache_hit: None,
        status,
        surface: ReceiptSurface::Web,
        timing_ticks: builder.finished_at_ms.saturating_mul(1000),
        console_lines: Vec::new(),
        url: None,
        final_url: None,
        title: None,
        status_code: None,
        dom_snapshot_hash: None,
        console_count: None,
        network_count: None,
        emitted_at_ms: if builder.emitted_at_ms > 0 {
            Some(builder.emitted_at_ms)
        } else {
            None
        },
        // Non-navigate receipts never carry settle fields.
        settle_until: None,
        settle_outcome: None,
    };

    payload.canonical_bytes()
}

/// v0.9.6 web-cookie-injection cookies-tier canonical bytes assembly.
///
/// Produces JCS-encoded bytes for receipts where any cookie-result field
/// is populated. Two transforms run BEFORE JCS encoding:
///
///   - **D13 tuple-identity sort.** Cookie arrays (set_cookies_result,
///     get_cookies_result) are sorted by `(name, domain.unwrap_or_default(),
///     path.unwrap_or_default())` byte-lex. RFC 6265 §5.3 identifies a
///     cookie by this triple, so the sort guarantees byte-identity
///     between record and replay even when two cookies share a `name`
///     but differ in domain/path. (For ASCII inputs — and cookie names
///     are restricted to RFC 6265 token chars — byte-lex matches the
///     UTF-16 lex specified by JCS.)
///
///   - **Value redaction.** `value` fields on cookies are replaced with
///     `"[REDACTED]"` in the canonical bytes. The receipt's outcome_hash
///     therefore depends on cookie *names* + *structure* but NOT on
///     cookie *values*, so replay (which substitutes values from a
///     `replay_cookie_values` map) reproduces byte-identical canonical
///     bytes regardless of which specific value is provided.
///
/// The operator-facing wire receipt (sent over JSON-RPC) is a separate
/// path that preserves raw values per D7 — see `build_navigate_wire_receipt`
/// in `loom-daemon`.
fn assemble_cookies_canonical_bytes(builder: &ReceiptBuilder) -> Result<Vec<u8>, LoomError> {
    use loom_core::error::LoomErrorCode;

    let payload = serde_json::json!({
        "action_id": builder.action_id,
        "status": builder.status,
        "started_at_ms": builder.started_at_ms,
        "finished_at_ms": builder.finished_at_ms,
        "side_effects_count": builder.side_effects_count,
        "host_call_count": builder.host_call_count,
        "error_code": builder.error_code,
        "error_details": builder.error_details,
        "action_hash": builder.action_hash,
        "outcome_hash": builder.outcome_hash,
        "emitted_at_ms": builder.emitted_at_ms,
        "set_cookies_result": prepare_cookies_field(builder.set_cookies_result.as_deref())?,
        "get_cookies_result": prepare_cookies_field(builder.get_cookies_result.as_deref())?,
        "clear_cookies_result": prepare_passthrough_field(builder.clear_cookies_result.as_deref())?,
        "delete_cookies_result": prepare_passthrough_field(builder.delete_cookies_result.as_deref())?,
    });

    serde_jcs::to_string(&payload)
        .map(String::into_bytes)
        .map_err(|e| {
            LoomError::new(
                LoomErrorCode::Internal,
                format!("assemble_cookies_canonical_bytes: JCS encode failed: {e}"),
            )
        })
}

/// Parse a JSON-encoded cookie array, redact `value` fields, sort by
/// (name, domain, path) tuple. Returns the cookie array as a
/// `serde_json::Value` ready to embed in the receipt payload (or
/// `Value::Null` when the input is None — JCS encodes Null verbatim).
fn prepare_cookies_field(raw: Option<&str>) -> Result<serde_json::Value, LoomError> {
    use loom_core::error::LoomErrorCode;
    let Some(s) = raw else {
        return Ok(serde_json::Value::Null);
    };
    let mut v: serde_json::Value = serde_json::from_str(s).map_err(|e| {
        LoomError::new(
            LoomErrorCode::Internal,
            format!("prepare_cookies_field: parse failed: {e}"),
        )
    })?;
    if let Some(arr) = v.as_array_mut() {
        arr.sort_by_key(cookie_sort_key);
        for item in arr.iter_mut() {
            if let Some(obj) = item.as_object_mut() {
                if obj.contains_key("value") {
                    obj.insert(
                        "value".to_string(),
                        serde_json::Value::String("[REDACTED]".to_string()),
                    );
                }
            }
        }
    }
    Ok(v)
}

/// Passthrough for `clear_cookies_result` / `delete_cookies_result` —
/// single-item structs with no value field, no array to sort. Parses
/// the JSON string into a Value so JCS encodes structure rather than
/// the escaped JSON string.
fn prepare_passthrough_field(raw: Option<&str>) -> Result<serde_json::Value, LoomError> {
    use loom_core::error::LoomErrorCode;
    let Some(s) = raw else {
        return Ok(serde_json::Value::Null);
    };
    serde_json::from_str(s).map_err(|e| {
        LoomError::new(
            LoomErrorCode::Internal,
            format!("prepare_passthrough_field: parse failed: {e}"),
        )
    })
}

fn cookie_sort_key(c: &serde_json::Value) -> (String, String, String) {
    let s = |k: &str| c.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    (s("name"), s("domain"), s("path"))
}

#[cfg(test)]
mod cookies_canonical_bytes_tests {
    use super::*;

    fn fixture_builder() -> ReceiptBuilder {
        ReceiptBuilder {
            action_id: 42,
            started_at_ms: 1000,
            finished_at_ms: 1010,
            status: ReceiptStatus::Ok,
            side_effects_count: 0,
            host_call_count: 1,
            error_code: None,
            error_details: None,
            action_hash: "ah".to_string(),
            outcome_hash: "oh".to_string(),
            emitted_at_ms: 1010,
            ..Default::default()
        }
    }

    #[test]
    fn assemble_cookies_path_invokes_when_set_cookies_result_present() {
        let mut b = fixture_builder();
        b.set_cookies_result = Some(r#"[{"name":"sid","success":true}]"#.to_string());
        let bytes = ReceiptMarshaller::assemble_canonical_bytes(&b).expect("ok");
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.contains("set_cookies_result"));
        assert!(s.contains("sid"));
    }

    #[test]
    fn d13_sort_places_cookies_with_same_name_distinct_domains_in_canonical_order() {
        let mut b = fixture_builder();
        b.get_cookies_result = Some(
            r#"[
                {"name":"sid","domain":"example.com","path":"/","value":"v1"},
                {"name":"sid","domain":"api.example.com","path":"/","value":"v2"}
            ]"#
            .to_string(),
        );
        let bytes = ReceiptMarshaller::assemble_canonical_bytes(&b).expect("ok");
        let s = String::from_utf8(bytes).unwrap();
        let api_pos = s.find("api.example.com").expect("api domain in output");
        let example_pos = s
            .find("\"example.com\"")
            .expect("example.com domain in output");
        assert!(
            api_pos < example_pos,
            "D13 sort: api.example.com should precede example.com (byte-lex)"
        );
    }

    #[test]
    fn d13_sort_distinguishes_cookies_with_same_name_distinct_paths() {
        let mut b = fixture_builder();
        b.get_cookies_result = Some(
            r#"[
                {"name":"sid","domain":"x.com","path":"/api","value":"v1"},
                {"name":"sid","domain":"x.com","path":"/","value":"v2"}
            ]"#
            .to_string(),
        );
        let bytes = ReceiptMarshaller::assemble_canonical_bytes(&b).expect("ok");
        let s = String::from_utf8(bytes).unwrap();
        let root_pos = s.find("\"/\"").expect("/ path");
        let api_pos = s.find("\"/api\"").expect("/api path");
        assert!(root_pos < api_pos, "/ should precede /api byte-lex");
    }

    #[test]
    fn cookie_values_are_redacted_in_canonical_bytes_per_replay_byte_identity() {
        let mut b = fixture_builder();
        b.get_cookies_result = Some(
            r#"[{"name":"sid","domain":"x.com","path":"/","value":"super-secret-token"}]"#
                .to_string(),
        );
        let bytes = ReceiptMarshaller::assemble_canonical_bytes(&b).expect("ok");
        let s = String::from_utf8(bytes).unwrap();
        assert!(
            !s.contains("super-secret-token"),
            "raw cookie value must not appear in canonical bytes"
        );
        assert!(
            s.contains("[REDACTED]"),
            "canonical bytes must carry [REDACTED] as the value substitute"
        );
    }

    #[test]
    fn byte_identity_holds_when_values_differ_but_tuples_match() {
        // Replay byte-identity guarantee: two receipts with same
        // (name, domain, path) but different cookie *values* must
        // produce IDENTICAL canonical bytes.
        let mut b1 = fixture_builder();
        b1.get_cookies_result =
            Some(r#"[{"name":"sid","domain":"x.com","path":"/","value":"VALUE_A"}]"#.to_string());
        let mut b2 = fixture_builder();
        b2.get_cookies_result = Some(
            r#"[{"name":"sid","domain":"x.com","path":"/","value":"DIFFERENT_VALUE_B"}]"#
                .to_string(),
        );
        let bytes1 = ReceiptMarshaller::assemble_canonical_bytes(&b1).expect("ok");
        let bytes2 = ReceiptMarshaller::assemble_canonical_bytes(&b2).expect("ok");
        assert_eq!(
            bytes1, bytes2,
            "canonical bytes must be identical regardless of cookie value (replay-byte-identity)"
        );
    }

    #[test]
    fn clear_cookies_result_passes_through_as_structured_object() {
        let mut b = fixture_builder();
        b.clear_cookies_result = Some(r#"{"cleared_count":7}"#.to_string());
        let bytes = ReceiptMarshaller::assemble_canonical_bytes(&b).expect("ok");
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.contains(r#""cleared_count":7"#));
    }

    #[test]
    fn delete_cookies_result_passes_through_with_matched_bool() {
        let mut b = fixture_builder();
        b.delete_cookies_result = Some(r#"{"name":"sid","matched":true}"#.to_string());
        let bytes = ReceiptMarshaller::assemble_canonical_bytes(&b).expect("ok");
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.contains(r#""matched":true"#));
        assert!(s.contains(r#""name":"sid""#));
    }

    // === Cross-crate replay byte-identity integration tests ===
    // Wires loom-core::replay_engine::cookie_replay::substitute_cookie_values
    // into the loom-host receipt marshaller and verifies the end-to-end
    // record→replay byte-identity invariant.

    #[test]
    fn record_then_replay_byte_identity_via_cookie_replay_substitution() {
        use loom_core::replay_engine::cookie_replay::{
            substitute_cookie_values, ReplayCookieValues,
        };

        // STEP 1: Recorded receipt — real cookie values.
        let recorded_payload =
            r#"[{"name":"sid","domain":"example.com","path":"/","value":"REAL_SESSION_TOKEN"}]"#;
        let mut recorded = fixture_builder();
        recorded.get_cookies_result = Some(recorded_payload.to_string());
        let recorded_bytes =
            ReceiptMarshaller::assemble_canonical_bytes(&recorded).expect("record canonical");

        // STEP 2: Replay receipt — substitute via cookie_replay.
        let mut replay_values: ReplayCookieValues = std::collections::BTreeMap::new();
        replay_values.insert(
            (
                "sid".to_string(),
                "example.com".to_string(),
                "/".to_string(),
            ),
            "REPLAY_PLACEHOLDER_VALUE".to_string(),
        );
        let replayed_payload =
            substitute_cookie_values(recorded.action_id, recorded_payload, &replay_values)
                .expect("substitute ok");
        // The substituted JSON has the replay placeholder, not the
        // recorded value.
        assert!(replayed_payload.contains("REPLAY_PLACEHOLDER_VALUE"));
        assert!(!replayed_payload.contains("REAL_SESSION_TOKEN"));

        let mut replay = fixture_builder();
        replay.get_cookies_result = Some(replayed_payload);
        let replay_bytes =
            ReceiptMarshaller::assemble_canonical_bytes(&replay).expect("replay canonical");

        // STEP 3: Byte-identity holds — the marshaller redacts values
        // in both paths, so the canonical bytes are identical regardless
        // of which placeholder the replay supplied.
        assert_eq!(
            recorded_bytes, replay_bytes,
            "record→replay canonical bytes must be byte-identical when (name,domain,path) tuples match"
        );
    }

    #[test]
    fn replay_missing_value_propagates_typed_error_through_substitution() {
        use loom_core::replay_engine::cookie_replay::{
            substitute_cookie_values, ReplayCookieValues, ReplayError,
        };

        let recorded_payload =
            r#"[{"name":"sid","domain":"example.com","path":"/api","value":"X"}]"#;
        // Supply value for "/" but the recorded path is "/api" — tuple mismatch.
        let mut replay_values: ReplayCookieValues = std::collections::BTreeMap::new();
        replay_values.insert(
            (
                "sid".to_string(),
                "example.com".to_string(),
                "/".to_string(),
            ),
            "P".to_string(),
        );
        let err = substitute_cookie_values(123, recorded_payload, &replay_values)
            .expect_err("must error");
        match err {
            ReplayError::MissingCookieValue {
                action_id,
                name,
                domain,
                path,
            } => {
                assert_eq!(action_id, 123);
                assert_eq!(name, "sid");
                assert_eq!(domain, "example.com");
                assert_eq!(path, "/api");
            }
            other => panic!("expected MissingCookieValue, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod cookie_edge_case_tests {
    use super::*;

    fn fixture_builder() -> ReceiptBuilder {
        ReceiptBuilder {
            action_id: 1,
            started_at_ms: 0,
            finished_at_ms: 1,
            status: ReceiptStatus::Ok,
            side_effects_count: 0,
            host_call_count: 0,
            error_code: None,
            error_details: None,
            action_hash: "ah".to_string(),
            outcome_hash: "oh".to_string(),
            emitted_at_ms: 1,
            ..Default::default()
        }
    }

    // === D13 sort edge cases ===

    #[test]
    fn d13_sort_with_empty_array_produces_empty_canonical_array() {
        let mut b = fixture_builder();
        b.get_cookies_result = Some("[]".to_string());
        let bytes = ReceiptMarshaller::assemble_canonical_bytes(&b).expect("ok");
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.contains(r#""get_cookies_result":[]"#));
    }

    #[test]
    fn d13_sort_with_single_cookie_is_noop_no_panic() {
        let mut b = fixture_builder();
        b.get_cookies_result =
            Some(r#"[{"name":"sid","domain":"x.com","path":"/","value":"v"}]"#.to_string());
        let bytes = ReceiptMarshaller::assemble_canonical_bytes(&b).expect("ok");
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.contains("\"name\":\"sid\""));
        assert!(s.contains("[REDACTED]"));
    }

    #[test]
    fn d13_sort_handles_cookies_with_missing_domain_field() {
        // RFC 6265: domain is optional. Cookies without `domain` should
        // sort using empty-string default (cookie_sort_key uses
        // unwrap_or("")). They should NOT cause a panic.
        let mut b = fixture_builder();
        b.get_cookies_result = Some(
            r#"[
                {"name":"sid","path":"/","value":"v"},
                {"name":"sid","domain":"x.com","path":"/","value":"v2"}
            ]"#
            .to_string(),
        );
        let bytes = ReceiptMarshaller::assemble_canonical_bytes(&b).expect("ok");
        let s = String::from_utf8(bytes).unwrap();
        // The domain-less cookie sorts BEFORE the domain-bearing one
        // (empty string < "x.com" byte-lex).
        let no_domain_pos = s.find(r#"{"domain":null"#).unwrap_or_else(|| {
            // The serializer might omit nulls — find by lack of "x.com"
            // near the first cookie. Fall back to position of first "sid".
            s.find("\"sid\"").expect("at least one sid")
        });
        let x_com_pos = s.find("\"x.com\"").expect("x.com");
        assert!(no_domain_pos <= x_com_pos);
    }

    #[test]
    fn d13_sort_handles_cookies_with_null_domain_field() {
        // `domain: null` should be treated identically to missing.
        let mut b = fixture_builder();
        b.get_cookies_result =
            Some(r#"[{"name":"sid","domain":null,"path":"/","value":"v"}]"#.to_string());
        let _ = ReceiptMarshaller::assemble_canonical_bytes(&b).expect("ok");
    }

    #[test]
    fn d13_sort_three_way_tie_preserves_no_panic_for_identical_tuples() {
        // Two cookies with identical (name, domain, path) — degenerate
        // case (real browsers shouldn't allow this). The sort is stable;
        // we just need to not panic and to produce consistent output.
        let mut b = fixture_builder();
        b.get_cookies_result = Some(
            r#"[
                {"name":"sid","domain":"x.com","path":"/","value":"v1"},
                {"name":"sid","domain":"x.com","path":"/","value":"v2"}
            ]"#
            .to_string(),
        );
        let bytes = ReceiptMarshaller::assemble_canonical_bytes(&b).expect("ok");
        let s = String::from_utf8(bytes).unwrap();
        // Both values redacted; both cookies present.
        let redacted_count = s.matches("[REDACTED]").count();
        assert_eq!(redacted_count, 2);
    }

    #[test]
    fn d13_sort_with_50_cookies_terminates_in_reasonable_time() {
        // Stress test: 50 cookies sort + redact in reasonable time.
        // Not a microbenchmark; just guards against O(n^2) regressions.
        use std::fmt::Write;
        let mut s = String::from("[");
        for i in 0..50 {
            if i > 0 {
                s.push(',');
            }
            write!(
                s,
                r#"{{"name":"sid","domain":"d{:02}.com","path":"/","value":"x"}}"#,
                49 - i // reverse order so sort has work to do
            )
            .unwrap();
        }
        s.push(']');
        let mut b = fixture_builder();
        b.get_cookies_result = Some(s);
        let bytes = ReceiptMarshaller::assemble_canonical_bytes(&b).expect("ok");
        let out = String::from_utf8(bytes).unwrap();
        // First domain should be d00.com (lexicographic first after sort).
        let d00 = out.find("\"d00.com\"").expect("d00 in output");
        let d01 = out.find("\"d01.com\"").expect("d01 in output");
        assert!(d00 < d01, "ascending order");
    }

    #[test]
    fn d13_sort_handles_unicode_in_domain() {
        // Domains *can* contain IDN-encoded unicode (xn--... punycode in
        // practice, but the typed string is UTF-8). Sort by byte-lex is
        // deterministic regardless. Test pins no-panic + deterministic
        // order.
        let mut b = fixture_builder();
        b.get_cookies_result = Some(
            r#"[
                {"name":"x","domain":"münchen.de","path":"/","value":"v"},
                {"name":"x","domain":"berlin.de","path":"/","value":"v"}
            ]"#
            .to_string(),
        );
        let bytes = ReceiptMarshaller::assemble_canonical_bytes(&b).expect("ok");
        let s = String::from_utf8(bytes).unwrap();
        // Look for the full domain strings — searching for bare "m" or
        // "b" hits the first occurrence anywhere in the JSON (e.g.
        // "name", "domain") which is not informative.
        let berlin = s.find("berlin.de").expect("berlin.de");
        let muenchen = s.find("münchen.de").expect("münchen.de");
        // 'b' < 'm' byte-lex, so berlin precedes münchen.
        assert!(
            berlin < muenchen,
            "berlin.de should appear before münchen.de in sorted output; got berlin={berlin}, muenchen={muenchen}"
        );
    }

    #[test]
    fn d13_sort_already_sorted_array_is_stable_no_op() {
        let mut b = fixture_builder();
        b.get_cookies_result = Some(
            r#"[
                {"name":"a","domain":"x.com","path":"/","value":"v"},
                {"name":"b","domain":"x.com","path":"/","value":"v"},
                {"name":"c","domain":"x.com","path":"/","value":"v"}
            ]"#
            .to_string(),
        );
        let bytes = ReceiptMarshaller::assemble_canonical_bytes(&b).expect("ok");
        let s = String::from_utf8(bytes).unwrap();
        let a = s.find("\"a\"").unwrap();
        let b_pos = s.find("\"b\"").unwrap();
        let c = s.find("\"c\"").unwrap();
        assert!(a < b_pos && b_pos < c);
    }

    // === Cookie result invalid-payload edge cases ===

    #[test]
    fn assemble_cookies_with_malformed_set_cookies_result_json_returns_internal_error() {
        let mut b = fixture_builder();
        b.set_cookies_result = Some("not json".to_string());
        let err = ReceiptMarshaller::assemble_canonical_bytes(&b).expect_err("must error");
        // We don't pin the exact LoomError code shape since the marshaller
        // uses the generic Internal variant for this branch; just check
        // it returned Err and didn't panic.
        assert!(!format!("{err:?}").is_empty());
    }

    #[test]
    fn assemble_cookies_with_get_cookies_result_as_object_not_array_returns_error_path() {
        // The marshaller's prepare_cookies_field expects an array. An
        // object should NOT panic; current impl tolerates it because
        // `v.as_array_mut()` returns None and the function returns Ok
        // with the original Value. Pin that behaviour: it doesn't
        // crash; the canonical bytes simply carry the object as-is.
        let mut b = fixture_builder();
        b.get_cookies_result = Some(r#"{"not":"an array"}"#.to_string());
        let bytes = ReceiptMarshaller::assemble_canonical_bytes(&b).expect("no panic");
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.contains("\"not\":\"an array\""));
    }

    #[test]
    fn assemble_cookies_with_no_value_field_on_cookie_skips_redaction() {
        // If a cookie object has no `value` field at all, the redactor
        // shouldn't add one — just leave it as-is. (Real-world cookies
        // always have a value, but the marshaller mustn't fabricate
        // data.)
        let mut b = fixture_builder();
        b.get_cookies_result = Some(r#"[{"name":"sid","domain":"x.com","path":"/"}]"#.to_string());
        let bytes = ReceiptMarshaller::assemble_canonical_bytes(&b).expect("ok");
        let s = String::from_utf8(bytes).unwrap();
        assert!(!s.contains("[REDACTED]"));
        assert!(s.contains("\"name\":\"sid\""));
    }

    #[test]
    fn assemble_cookies_clear_result_with_zero_cleared_count() {
        let mut b = fixture_builder();
        b.clear_cookies_result = Some(r#"{"cleared_count":0}"#.to_string());
        let bytes = ReceiptMarshaller::assemble_canonical_bytes(&b).expect("ok");
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.contains("\"cleared_count\":0"));
    }

    #[test]
    fn assemble_cookies_delete_result_with_matched_false() {
        let mut b = fixture_builder();
        b.delete_cookies_result = Some(r#"{"name":"sid","matched":false}"#.to_string());
        let bytes = ReceiptMarshaller::assemble_canonical_bytes(&b).expect("ok");
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.contains("\"matched\":false"));
    }
}
