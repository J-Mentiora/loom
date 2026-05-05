// Re-export of the locked v5.3 interface. DO NOT EDIT here.
// Edit `systems/loom-host/modules/receipt_marshaller/interfaces.rs` instead.
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
// - **Backpressure.** Background queue depth 256; full → synchronous
//   `ManifestWriter::append` on the calling task (rare, soft binding).

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
    pub(crate) queue_depth: usize,
}

impl ReceiptMarshaller {
    pub fn new(
        manifest_writer: Arc<dyn ManifestWriter>,
        budget: Arc<dyn loom_core::budget_enforcer::BudgetEnforcer>,
    ) -> Arc<Self> {
        Arc::new(Self {
            manifest_writer,
            budget,
            queue_depth: 256,
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
    pub fn assemble_canonical_bytes(
        builder: &ReceiptBuilder,
    ) -> Result<Vec<u8>, LoomError> {
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
    };

    payload.canonical_bytes()
}
