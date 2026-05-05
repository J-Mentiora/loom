// Re-export of the locked v5.3 interface. DO NOT EDIT here.
// Edit `systems/loom-rpc/modules/host_service_adapter/interfaces.rs` instead.
// HostServiceAdapter — routes `action.<surface>.<verb>` methods to
// `loom-host::WasmHost::dispatch`.
//
// # Contract semantics
// - **Single dispatch point.** Every action method
//   translates to a single `WasmHost::dispatch(action).await` call.
//   This adapter awaits the host future on the connection's tokio
//   task — no extra spawn.
// - **No CDP bytes.** This adapter receives
//   only typed `Receipt` values from `WasmHost::dispatch`. CDP
//   translation lives inside `loom-host::ReceiptMarshaller`. Any
//   code path here that touched `serde_json::Value` of CDP shape
//   would be a structural violation; this is enforced by the typed
//   `Action` / `Receipt` Rust signatures emitted by wit-bindgen.
// - **Latency partition.** The await on
//   `WasmHost::dispatch` is the single boundary recorded as
//   `host_dispatch_us` by `RpcObservability`; that interval is
//   excluded from the RPC-overhead budget.
// - **Error mapping.** `LoomError` returned by the host
//   is propagated up; `RpcHandlers` invokes `ErrorTranslator`.

use loom_core::receipt_builder::receipt_builder::NetworkSummary;
use loom_shared::navigate_outcome::ShimConsoleLine;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// We reference `loom_host::WasmHost` through a bridge trait so this
// module is testable without pulling the loom-host crate into unit
// tests.

/// Marker trait satisfied by `loom_host::WasmHost`. The adapter holds
/// `Arc<dyn WasmHostBridge>` for testability; production wiring
/// resolves to `Arc<loom_host::WasmHost>` directly.
pub trait WasmHostBridge: Send + Sync {
    /// Dispatch an action to the WASM surface. Returns a typed
    /// `Receipt` (CDP-free, per the contract). This is the one and only
    /// host-side entry point.
    fn dispatch_action_blocking(&self, action: Action) -> Result<Receipt, AdapterError>;

    /// true iff a chromium template was registered at host boot. False
    /// when the chromium_resolver returned `BrowserNotFound` at daemon
    /// boot. The default `true` keeps unit-test bridges working;
    /// production impls (`WasmBridge`, `StubHostBridge`) override.
    fn has_chromium(&self) -> bool {
        true
    }
}

/// WIT-derived action type. The variant names mirror the
/// `action.<surface>.<verb>` method-list block from
/// `wit/loom-surface.wit`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    WebNavigate {
        session_id: String,
        url: String,
    },
    WebClick {
        session_id: String,
        selector: String,
    },
    WebEvaluate {
        session_id: String,
        expression: String,
    },
    WebType {
        session_id: String,
        selector: String,
        text: String,
    },
    WebScreenshot {
        session_id: String,
        selector: Option<String>,
    },
    WebSelect {
        session_id: String,
        selector: String,
        value: String,
    },
    WebHover {
        session_id: String,
        selector: String,
    },
    WebScroll {
        session_id: String,
        selector: String,
        delta_x: Option<i64>,
        delta_y: Option<i64>,
    },
    WebWait {
        session_id: String,
        selector: String,
        timeout_ms: Option<u64>,
    },
    WebSnapshot {
        session_id: String,
    },
    // Additional surface.verb pairs added as the WIT grows. The match
    // arm in `RpcHandlers` is exhaustive — adding a verb forces a
    // handler addition (compile-time evidence).
}

/// WIT-derived receipt type. Always typed; never CDP-shaped
/// . `serde_json::Value` here is a domain payload (e.g.
/// click coordinates), NOT the CDP wire envelope.
///
/// `action_hash`, `outcome_hash`, `emitted_at_ms` mirror the WIT
/// `record receipt` fields populated by the WASM guest. Optional so
/// fixture/canned receipts (and trap-path receipts that never reach
/// the guest) can leave them absent in the JSON output.
///
/// Navigate tier-2 fields : present only when the
/// receipt was produced by a navigate action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub action_id: u64,
    pub session_id: String,
    pub status: ReceiptStatus,
    pub timing_ticks: u64,
    pub side_effects: Vec<serde_json::Value>,
    pub error: Option<ReceiptError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emitted_at_ms: Option<u64>,
    // ---- Navigate tier-2 fields  ----
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dom_snapshot_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot_after_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub console_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_count: Option<u64>,
    /// Per-line console output captured during navigate (brief
    /// AC-NAVRECEIPT2-01 extension). Empty when capture-policy is
    /// `minimal`, when no console output occurred, or while the shim
    /// console-capture stub is in place. Reuses `ShimConsoleLine`
    /// across the wire boundary — the shape `{level, message}` is
    /// identical and an extra newtype would only churn the call
    /// graph.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub console_lines: Vec<ShimConsoleLine>,
    /// Aggregate network summary (brief AC-NAVRECEIPT2-01 extension).
    /// Per-request detail lives in `side_effects[]`; this carries the
    /// roll-up so consumers don't need to scan the array.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_summary: Option<NetworkSummary>,
    // ---- Evaluate tier fields  ----
    /// JS expression result, canonical-JSON encoded. `None` means either
    /// "not an evaluate action" or "result was offloaded to the content
    /// store" (in which case `return_value_blob_ref` carries the SHA-256).
    ///.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_value_json: Option<String>,
    /// SHA-256 hex of the canonical-JSON evaluate result when its size
    /// exceeds the inline threshold (64 KB by default). `None` for
    /// inline-sized results..
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_value_blob_ref: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    Success,
    Error,
    Aborted,
}

/// Wire-shape error payload on a `Receipt`. `kind` is a stable typed
/// string (e.g. `"http_status"`, `"dns_failure"`, `"connect_refused"`,
/// `"tls_error"`, `"shim_failure"`); `detail` carries kind-specific
/// fields (e.g. `{status_code, url}` for `http_status`, `{url,
/// chromium_error}` for transport-layer kinds). `detail` is `None` for
/// kinds that have no kind-specific data...03 specify
/// this exact shape for navigate receipts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiptError {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

pub type AdapterError = crate::error_translator::error_translator::LoomErrorCode;

/// Trait surface so `RpcHandlers` can be unit-tested with a fake host.
#[async_trait::async_trait]
pub trait HostServiceAdapterApi: Send + Sync {
    /// Single dispatch entry point . Awaits the host
    /// future on the caller's task (no extra spawn).
    /// The interval of this await is recorded as `host_dispatch_us`
    /// and excluded from the budget .
    async fn dispatch_action(&self, action: Action) -> Result<Receipt, AdapterError>;

    /// true iff a chromium template was registered at host
    /// boot. False when the chromium_resolver returned `BrowserNotFound`
    /// or `current_exe()` failed (no shim_chromium config). Consumed by
    /// `session_create` to fail-fast with `BrowserNotFound` before any
    /// SessionInfo is constructed.
    fn has_chromium(&self) -> bool {
        true
    }
}

#[allow(dead_code)]
pub struct HostServiceAdapter {
    pub(crate) host: Arc<dyn WasmHostBridge>,
}

impl HostServiceAdapter {
    pub fn new(host: Arc<dyn WasmHostBridge>) -> Arc<Self> {
        Arc::new(Self { host })
    }
}
