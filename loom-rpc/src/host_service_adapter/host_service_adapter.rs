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
    ///
    /// # Threading contract
    /// Called ONLY from a blocking thread (the adapter wraps it in
    /// `tokio::task::spawn_blocking`), never from an async worker.
    /// Implementations may therefore drive inner async work with a
    /// plain `Handle::block_on` — and must NOT use `block_in_place`,
    /// which panics off the runtime's worker threads.
    /// `deadline_ms` is the optional per-action kill deadline (dispatch
    /// metadata; NOT part of the `Action` identity, so it never enters the
    /// hashed action bytes). On expiry the executor returns a typed
    /// `request_timeout` trapped outcome.
    fn dispatch_action_blocking(
        &self,
        action: Action,
        deadline_ms: Option<u64>,
    ) -> Result<Receipt, AdapterError>;

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
        // settle-capture: readiness gate for the auto-capture. `None` = the
        // daemon default (`settled`). One of `load|networkidle|settled`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    WebClick {
        session_id: String,
        selector: String,
        // interactive-settle-bounded: post-action readiness gate, mirroring
        // WebNavigate. `None` == the daemon default (`settled`). One of
        // `load|networkidle|settled`. The wait is bounded inside the RPC
        // deadline so a never-settling page returns a `settle_outcome` receipt,
        // never a transport `rpc timeout`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<String>,
    },
    WebEvaluate {
        session_id: String,
        expression: String,
    },
    WebType {
        session_id: String,
        selector: String,
        text: String,
        // cdp-trusted-input: "value" (default — set element.value via
        // Runtime.evaluate + synthetic input/change events, `isTrusted:false`)
        // or "keystrokes" (focus + real per-char `Input.dispatchKeyEvent`,
        // `isTrusted:true`). `None` == "value" (back-compat / determinism).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<String>,
        // interactive-settle-bounded: post-action readiness gate for the
        // host-side `fill`/`keystrokes` paths (ignored for `value`). Same
        // semantics as WebClick::until.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<String>,
    },
    // cdp-trusted-input: dispatch a real key event (`isTrusted:true`) via CDP
    // `Input.dispatchKeyEvent`. `key` is a named key (Enter/Tab/Escape/arrows/…)
    // or a single printable char; `modifiers` are Control/Alt/Shift/Meta; the
    // optional `selector` focuses an element first (else ambient focus).
    // Host-side intercept — does NOT run the WASM guest.
    WebPressKey {
        session_id: String,
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        modifiers: Option<Vec<String>>,
    },
    WebScreenshot {
        session_id: String,
        selector: Option<String>,
    },
    // video-capture: bracketed screencast recording. start begins a CDP
    // Page.startScreencast on the active target; stop encodes the collected
    // frames to .webm and returns its content hash. At most one active per session.
    WebStartRecording {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_bytes: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frame_rate: Option<u64>,
    },
    WebStopRecording {
        session_id: String,
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
        // `selector` is OPTIONAL: omitted (or body/html/document) scrolls the
        // viewport (`document.scrollingElement`); a real CSS selector scrolls
        // that element. See `build_scroll_expression` in loom-daemon.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        delta_x: Option<i64>,
        delta_y: Option<i64>,
    },
    WebWait {
        session_id: String,
        selector: String,
        timeout_ms: Option<u64>,
    },
    // settle-capture slice 2: standalone readiness wait on the current page.
    // `until` is the readiness mode (`load|networkidle|settled`, default
    // `settled`); validated by the router's `optional_settle_until`.
    WebWaitFor {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    WebSnapshot {
        session_id: String,
    },
    // web.set_input_files: upload local files into an <input type=file>.
    // `paths` are absolute host paths; the daemon validates + canonicalizes
    // them against LOOM_UPLOAD_ROOT (upload_guard) BEFORE dispatch.
    WebSetInputFiles {
        session_id: String,
        selector: String,
        paths: Vec<String>,
    },
    // v0.9.6 web-cookie-injection: 4 cookie verbs. `source` for set_cookies
    // is the typed `CookieSource` JSON shape — `{"source":"inline","cookies":[...]}`
    // or `{"source":"grant","grant_id":"..."}`. Typed deserialization to
    // `loom_shared::cookie_types::CookieSource` happens on the daemon
    // side; `loom-rpc` keeps `source` untyped here.
    WebSetCookies {
        session_id: String,
        source: serde_json::Value,
    },
    WebGetCookies {
        session_id: String,
        urls: Option<Vec<String>>,
    },
    WebClearCookies {
        session_id: String,
    },
    WebDeleteCookies {
        session_id: String,
        name: String,
        url: Option<String>,
        domain: Option<String>,
        path: Option<String>,
    },
    // loom.web.network_log: read the session-accumulated network entries
    // observed since the last navigate (document + click-triggered xhr/fetch).
    // Observation-only; no CDP round-trip.
    WebNetworkLog {
        session_id: String,
    },
    // voice-call-io: browser voice-call I/O verbs (Architecture §2). SURFACE ONLY
    // in task 02 — all four are host-side intercepts (no direct-CDP envelope) and
    // their daemon/shim dispatch is wired in later tasks (03–05, 09). `deadline_ms`
    // is threaded out-of-band by the router and is never a field here.
    WebInjectAudio {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blob_ref: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audio_b64: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        await_playout: Option<bool>,
    },
    WebStartAudioCapture {
        session_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_bytes: Option<u64>,
    },
    WebStopAudioCapture {
        session_id: String,
    },
    WebSay {
        session_id: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        await_playout: Option<bool>,
    },
    // Additional surface.verb pairs added as the WIT grows. The match
    // arm in `RpcHandlers` is exhaustive — adding a verb forces a
    // handler addition (compile-time evidence).
}

/// WIT-derived receipt type. Always typed; never CDP-shaped.
/// `serde_json::Value` here is a domain payload (e.g.
/// click coordinates), NOT the CDP wire envelope.
///
/// `action_hash`, `outcome_hash`, `emitted_at_ms` mirror the WIT
/// `record receipt` fields populated by the WASM guest. Optional so
/// fixture/canned receipts (and trap-path receipts that never reach
/// the guest) can leave them absent in the JSON output.
///
/// Navigate tier-2 fields: present only when the
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
    /// Interaction-tier (`capture-policy=fingerprint`) post-action DOM
    /// fingerprint: sha256 of the normalized post-action DOM for DOM-mutating
    /// selector verbs (click/type/select/hover). Distinct from the navigate
    /// `dom_snapshot_hash`; content-bearing (unlike the per-verb-constant
    /// `outcome_hash`). Absent under default/minimal/full.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dom_after_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot_after_hash: Option<String>,
    /// video-capture: SHA-256 of the recorded `.webm` in CAS, set on a
    /// `web.stop_recording` receipt. Fetch via `loom blob get <hash>`. The webm
    /// bytes live OUTSIDE the manifest hash chain (recording is intercepted
    /// host-side, like `web.network_log`), so this never affects replay-equality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screencast_after_hash: Option<String>,
    /// voice-call-io: SHA-256 of the captured-call `.wav` in CAS, set on a
    /// `web.stop_audio_capture` receipt. Fetch via `loom blob get <hash>`. Like
    /// screencast, the audio bytes live OUTSIDE the manifest hash chain (capture is
    /// intercepted host-side), so this never affects replay-equality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_after_hash: Option<String>,
    /// voice-call-io: why capture stopped (`explicit` | `byte_cap` | `duration_cap`
    /// | `no_samples` | `no_inbound_track` | `session_closed` | `error`). Surfaced so
    /// a cap-truncated capture is caller-observable, not just a server log line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_stop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub console_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_count: Option<u64>,
    /// Per-line console output captured during navigate (brief
    ///  extension). Empty when capture-policy is
    /// `minimal`, when no console output occurred, or while the shim
    /// console-capture stub is in place. Reuses `ShimConsoleLine`
    /// across the wire boundary — the shape `{level, message}` is
    /// identical and an extra newtype would only churn the call
    /// graph.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub console_lines: Vec<ShimConsoleLine>,
    /// Aggregate network summary (brief  extension).
    /// Per-request detail lives in `side_effects[]`; this carries the
    /// roll-up so consumers don't need to scan the array.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_summary: Option<NetworkSummary>,
    /// Raw per-request network entries (full-capture: document + xhr/fetch +
    /// subresource), each `{url, method, status, resource_type, from_cache,
    /// request_id, ts_ms}`. OBSERVATIONAL — not part of the replay hash chain.
    /// Empty when offloaded to the CAS (see `network_entries_blob_ref`) or when
    /// stripped under `minimal` capture. Redirect hops share `request_id`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_entries: Vec<serde_json::Value>,
    /// SHA-256 hex of the canonical-JSON `network_entries` blob when the
    /// serialized list ≥ 64KB. `None` when inline-sized or dropped on offload
    /// failure. Mutually exclusive with a non-empty `network_entries`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_entries_blob_ref: Option<String>,
    /// True when `network_entries` is incomplete — the cap was hit OR a
    /// content-store offload failed and the list was dropped. To disambiguate:
    /// `blob_ref=None && network_entries empty && truncated` ⇒ offload failure;
    /// otherwise a cap hit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_entries_truncated: Option<bool>,
    // ---- settle-capture readiness fields (navigate tier) ----
    /// Readiness mode the capture was gated on (`load|networkidle|settled`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle_until: Option<String>,
    /// How the readiness wait ended (`reached|timeout|dom_unstable`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle_outcome: Option<String>,
    // ---- Evaluate tier fields ----
    /// JS expression result, canonical-JSON encoded. `None` means either
    /// "not an evaluate action" or "result was offloaded to the content
    /// store" (in which case `return_value_blob_ref` carries the SHA-256).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_value_json: Option<String>,
    /// SHA-256 hex of the canonical-JSON evaluate result when its size
    /// exceeds the inline threshold (64 KB by default). `None` for
    /// inline-sized results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_value_blob_ref: Option<String>,
    // ---- Cookie tier fields (v0.9.6 web-cookie-injection) ----
    /// `Vec<SetCookieResult>` from `web.set_cookies`. Each entry is shape
    /// `{"name":..., "success":bool, "error_code":Option<String>}` per the
    /// `loom_shared::cookie_types::SetCookieResult` struct. Untyped here
    /// because `loom-rpc` keeps this value as raw JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_cookies_result: Option<serde_json::Value>,
    /// `Vec<NetworkCookie>` from `web.get_cookies`. Cookie values arrive
    /// here RAW per D7 (operator-facing receipts include values).
    /// Structured logs are scrubbed via `mcp_observability` JSONPaths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub get_cookies_result: Option<serde_json::Value>,
    /// `{"cleared_count": u32}` from `web.clear_cookies`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clear_cookies_result: Option<serde_json::Value>,
    /// `{"name": String, "matched": bool}` from `web.delete_cookies`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_cookies_result: Option<serde_json::Value>,
    // ---- Scroll tier field ----
    /// Post-scroll viewport position `{"x": <window.scrollX>, "y": <window.scrollY>}`
    /// from `web.scroll`. Surfaced by reusing the evaluate-execute host import
    /// (the guest `scroll_verb` runs the scroll JS via `Runtime.evaluate`); the
    /// daemon moves the value out of `return_value_json` into this field so a
    /// scroll receipt has a single, purpose-named source of truth
    /// (`return_value_json` stays `None` for scroll). The position is already
    /// hash-chained via `outcome_hash`; this field is wire-only (not in the
    /// canonical replay chain).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll_result: Option<serde_json::Value>,
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
    async fn dispatch_action(
        &self,
        action: Action,
        deadline_ms: Option<u64>,
    ) -> Result<Receipt, AdapterError>;

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
