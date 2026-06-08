// NetworkInterceptor — R1 mitigation owner.
//
// # Contract semantics
// - **R1 LOAD-BEARING ORDERING (KILL).** For each
//   `Network.responseReceived` event:
//     1. Call `Network.getResponseBody({requestId})` — chromiumoxide
//        returns the DECOMPRESSED body bytes. Chromium itself
//        decompresses gzip/br before exposing the body; the shim
//        does NOT need a separate decompression library.
//     2. Strip `Content-Encoding` header from the recorded request
//        descriptor.
//     3. Compute SHA-256 of the decompressed bytes.
//     4. Append a `LoomNetworkEvent` with the hash.
//   Hashing compressed bytes → KILL — replay parity broken.
// - **No CDP payload escape.** The emitted
//   `LoomNetworkEvent` carries typed fields only — method, URL,
//   request_hash, response_hash, status, content_type, duration_ms.
//   The raw `Network.responseReceived` CBOR object never leaves
//   `NetworkInterceptor`.
// - **Subscription is callback-based (acyclicity).**
//   `NetworkInterceptor::new` registers an event handler in
//   `CdpConnection`; `CdpConnection` does NOT import this module.
// - **Per-target accumulator.** Events are appended to a per-target
//   `Vec<LoomNetworkEvent>`; `ActionExecutor::page_navigate` drains
//   the vec when `Page.loadEventFired` arrives and includes the
//   events in the `ActionResult::Navigated`.
// - **Hard-binding 3 compliance.** `LoomNetworkEvent` has integer-only
//   numeric fields (durations in ms as u64, sizes in bytes as u64,
//   hashes as hex strings). No floats.

use crate::cdp_connection::cdp_connection::{
    CdpConnection, EventFilter, EventHandler, EventRegistration,
};
use crate::ipc_endpoint::ipc_endpoint::{CdpMessage, TargetId};
use ciborium::value::Value as CborValue;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub use loom_shared::navigate_outcome::{BlockedEvent, LoomNetworkEntry};

/// Length of a SHA-256 hex string. Used for compile-time sanity in tests.
pub const SHA256_HEX_LEN: usize = 64;

/// Typed network event emitted by the shim. Replaces the raw CDP
/// `Network.responseReceived` payload at the boundary.
/// All numeric fields are integers per Hard Binding 3.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoomNetworkEvent {
    pub method: String, // "GET" / "POST" / etc.
    pub url: String,
    /// SHA-256 of the canonical request descriptor (with
    /// Content-Encoding STRIPPED). Hex.
    pub request_hash: String,
    /// SHA-256 of the DECOMPRESSED response body. Hex. Empty string
    /// iff `error_reason.is_some()`.
    pub response_hash: String,
    pub status: u16,
    pub content_type: String,
    pub duration_ms: u64,
    pub response_bytes: u64, // size of decompressed body
    pub error_reason: Option<String>,
    /// Classification of `error_reason`, populated by the shim's
    /// `ChromiumActionExecutor::page_navigate` (via
    /// `classify_chromium_nav_error`) when the synthetic
    /// `error_reason`-bearing event is pushed. One of:
    /// `"dns_failure"`, `"connect_refused"`, `"tls_error"`,
    /// `"network_error"`. `None` for successful events.
    /// Serde-default-`None` so any pre-existing CBOR-encoded payload
    /// deserialises unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
}

impl LoomNetworkEvent {
    /// Whether this event represents a successful response (response_hash present).
    pub fn is_complete(&self) -> bool {
        self.error_reason.is_none() && self.response_hash.len() == SHA256_HEX_LEN
    }
}

/// Default cap on the per-session network-entries accumulator. Mirrors the
/// `SessionCreateOpts.max_network_entries` default; bounds in-memory growth for
/// long-lived sessions that issue many xhr/fetch.
pub const DEFAULT_MAX_NETWORK_ENTRIES: usize = 1000;

#[derive(Default)]
struct EntryAccState {
    /// Entries in first-observed order (one per redirect hop).
    entries: Vec<LoomNetworkEntry>,
    /// `request_id` → index of the MOST-RECENT entry/hop for that id, so
    /// `responseReceived` / `requestServedFromCache` update the right hop.
    last_index: std::collections::HashMap<String, usize>,
    /// Set once the cap is hit and further entries were dropped.
    truncated: bool,
}

/// Stateful CDP-event → `LoomNetworkEntry` correlator for the full-capture
/// (non-hashed, observational) network-entries path. UNLIKE
/// `parse_network_event` (the Document-only, hashed path), this captures EVERY
/// resource type — xhr/fetch/subresource/document — and extracts the HTTP
/// method from `requestWillBeSent`. Correlates the 4 relevant CDP events by
/// `requestId`. One accumulator per session/target; cleared at navigate START,
/// accumulating across in-session actions until the next navigate.
///
/// Interior-mutable (`observe` takes `&self`) so a single accumulator can live
/// behind the `Network.*` event-handler closure and be read concurrently.
pub struct NetworkEntryAccumulator {
    state: parking_lot::Mutex<EntryAccState>,
    cap: usize,
}

impl Default for NetworkEntryAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkEntryAccumulator {
    pub fn new() -> Self {
        Self::with_cap(DEFAULT_MAX_NETWORK_ENTRIES)
    }

    pub fn with_cap(cap: usize) -> Self {
        Self {
            state: parking_lot::Mutex::new(EntryAccState::default()),
            cap,
        }
    }

    /// Fold one CDP `Network.*` event into the accumulator. Ignores unrelated
    /// methods and malformed params (returns silently). Never reads bodies or
    /// headers — metadata only.
    pub fn observe(&self, method: &str, params: &CborValue) {
        let map = match params {
            CborValue::Map(entries) => entries,
            _ => return,
        };
        match method {
            "Network.requestWillBeSent" => {
                let request = match cbor_map_get(map, "request") {
                    Some(CborValue::Map(r)) => r,
                    _ => return,
                };
                let url = cbor_map_text(request, "url").unwrap_or_default();
                let http_method = cbor_map_text(request, "method").unwrap_or_default();
                let resource_type = cbor_map_text(map, "type").unwrap_or_default();
                let request_id = cbor_map_text(map, "requestId").unwrap_or_default();
                let ts_ms = cbor_map_f64(map, "wallTime")
                    .filter(|w| *w > 0.0)
                    .map(|w| (w * 1000.0) as u64)
                    .unwrap_or(0);
                // On a redirect, CDP fires a fresh requestWillBeSent for the new
                // hop carrying `redirectResponse` — the response (status) of the
                // PRIOR hop, under the SAME requestId. Backfill the prior hop's
                // status before pushing the new hop, so a 302→200 chain reads as
                // [302, 200] not [0, 200].
                let redirect_status = match cbor_map_get(map, "redirectResponse") {
                    Some(CborValue::Map(rr)) => cbor_map_u16(rr, "status"),
                    _ => None,
                };
                let mut st = self.state.lock();
                if let Some(status) = redirect_status {
                    if let Some(idx) = st.last_index.get(&request_id).copied() {
                        if let Some(e) = st.entries.get_mut(idx) {
                            if e.status == 0 {
                                e.status = status;
                            }
                        }
                    }
                }
                if st.entries.len() >= self.cap {
                    st.truncated = true;
                    return;
                }
                let idx = st.entries.len();
                st.entries.push(LoomNetworkEntry {
                    url,
                    method: http_method,
                    status: 0,
                    resource_type,
                    from_cache: false,
                    request_id: request_id.clone(),
                    ts_ms,
                });
                st.last_index.insert(request_id, idx);
            }
            "Network.responseReceived" => {
                let request_id = cbor_map_text(map, "requestId").unwrap_or_default();
                let resource_type = cbor_map_text(map, "type");
                let (status, from_cache, resp_url) = match cbor_map_get(map, "response") {
                    Some(CborValue::Map(r)) => {
                        let status = cbor_map_u16(r, "status").unwrap_or(0);
                        let from_cache = cbor_map_bool(r, "fromDiskCache").unwrap_or(false)
                            || cbor_map_bool(r, "fromServiceWorker").unwrap_or(false)
                            || cbor_map_bool(r, "fromPrefetchCache").unwrap_or(false);
                        (
                            status,
                            from_cache,
                            cbor_map_text(r, "url").unwrap_or_default(),
                        )
                    }
                    _ => (0, false, String::new()),
                };
                let mut st = self.state.lock();
                match st.last_index.get(&request_id).copied() {
                    Some(idx) => {
                        if let Some(e) = st.entries.get_mut(idx) {
                            e.status = status;
                            if let Some(rt) = resource_type {
                                if !rt.is_empty() {
                                    e.resource_type = rt;
                                }
                            }
                            e.from_cache = e.from_cache || from_cache;
                        }
                    }
                    // A response with no seen `requestWillBeSent` (e.g. the
                    // request was buffer-evicted) is still kept — the studio
                    // wants the complete list. Method is unknown (empty).
                    None if st.entries.len() < self.cap => {
                        let idx = st.entries.len();
                        st.entries.push(LoomNetworkEntry {
                            url: resp_url,
                            method: String::new(),
                            status,
                            resource_type: resource_type.unwrap_or_default(),
                            from_cache,
                            request_id: request_id.clone(),
                            ts_ms: 0,
                        });
                        st.last_index.insert(request_id, idx);
                    }
                    None => st.truncated = true,
                }
            }
            "Network.requestServedFromCache" => {
                let request_id = cbor_map_text(map, "requestId").unwrap_or_default();
                let mut st = self.state.lock();
                if let Some(idx) = st.last_index.get(&request_id).copied() {
                    if let Some(e) = st.entries.get_mut(idx) {
                        e.from_cache = true;
                    }
                }
            }
            // `loadingFailed` keeps the already-created entry (status stays 0,
            // meaning "no final response"); nothing to update. Unknown methods
            // are ignored.
            _ => {}
        }
    }

    /// Snapshot of accumulated entries in first-observed order (clones).
    pub fn snapshot(&self) -> Vec<LoomNetworkEntry> {
        self.state.lock().entries.clone()
    }

    /// Whether the cap was hit and entries were dropped.
    pub fn truncated(&self) -> bool {
        self.state.lock().truncated
    }

    /// Reset contents (keeps the cap). Called at navigate START.
    pub fn clear(&self) {
        let mut st = self.state.lock();
        st.entries.clear();
        st.last_index.clear();
        st.truncated = false;
    }
}

/// Concrete NetworkInterceptor.
pub struct ChromiumNetworkInterceptor {
    pub(crate) cdp: Arc<dyn CdpConnection>,
    pub(crate) per_target:
        parking_lot::RwLock<std::collections::BTreeMap<TargetId, Vec<LoomNetworkEvent>>>,
    pub(crate) registration: parking_lot::Mutex<Option<EventRegistration>>,
    /// Sub-resource requests blocked by the default blocklist.
    /// Drained by `ActionExecutor::page_navigate`
    /// after `Page.loadEventFired`; the host then writes one
    /// `AuditEntry { kind: BlockedUrl }` per event into the manifest
    /// hash chain.
    pub(crate) blocked_per_target:
        parking_lot::RwLock<std::collections::BTreeMap<TargetId, Vec<BlockedEvent>>>,
    /// Categorized blocklist patterns: `(category, pattern)` pairs from
    /// `parse_blocklist_with_categories`. Empty when blocklist
    /// enforcement is disabled — `subscribe()` becomes a no-op and the
    /// `Fetch.*` handler is never installed (constructor decides).
    pub(crate) blocklist: Arc<Vec<(String, String)>>,
    /// Set of `(target_id, frame_id)` for which the first
    /// `Fetch.requestPaused` event has already been processed. The
    /// first event for a given frame's top-frame document is the
    /// page-level navigate (operator's primary URL); skip-gate that
    /// one regardless of blocklist match (top-frame Document is the operator's primary navigation). All
    /// subsequent events on the same target are gated normally.
    pub(crate) top_frame_seen: parking_lot::RwLock<
        std::collections::BTreeMap<TargetId, std::collections::BTreeSet<String>>,
    >,
    /// Registration for the `Fetch.*` event handler (only present when
    /// `blocklist` is non-empty). Held alongside `registration` (the
    /// `Network.*` handler) so both stay alive for the interceptor's
    /// lifetime.
    pub(crate) fetch_registration: parking_lot::Mutex<Option<EventRegistration>>,
    /// Per-target FULL-capture accumulator (xhr/fetch/subresource/document)
    /// for the observational `network_entries` side-channel. Distinct from
    /// `per_target` (the Document-only hashed path). Read (not drained) for the
    /// navigate receipt; cleared at navigate START; read again by the
    /// `network_log` tool. One target per session, so per-target == per-session.
    /// The accumulator caps at `DEFAULT_MAX_NETWORK_ENTRIES` (a memory backstop
    /// in the shim subprocess); the host applies the per-session configurable
    /// `max_network_entries` on top when building the receipt.
    pub(crate) entries_per_target:
        parking_lot::RwLock<std::collections::BTreeMap<TargetId, NetworkEntryAccumulator>>,
}

impl ChromiumNetworkInterceptor {
    /// Legacy constructor — back-compat alias for
    /// `new_with_blocklist(cdp, vec![])`. Preserves the
    /// pre-blocklist-enforcement behavior: registers only the `Network.*` event observer; no
    /// `Fetch.enable` is issued, no sub-resource gating happens.
    /// `subscribe(target_id)` becomes a no-op.
    pub fn new(cdp: Arc<dyn CdpConnection>) -> Arc<Self> {
        Self::new_with_blocklist(cdp, Vec::new())
    }

    /// Construct + register the `Network.*` event observer (always),
    /// plus a `Fetch.*` handler when `blocklist` is non-empty
    /// (the blocklist enforcement path). The registrations are the ONLY
    /// edges between these modules; no import of `CdpConnection`
    /// happens in the reverse direction.
    pub fn new_with_blocklist(
        cdp: Arc<dyn CdpConnection>,
        blocklist: Vec<(String, String)>,
    ) -> Arc<Self> {
        let s = Arc::new(Self {
            cdp: cdp.clone(),
            per_target: parking_lot::RwLock::new(Default::default()),
            registration: parking_lot::Mutex::new(None),
            blocked_per_target: parking_lot::RwLock::new(Default::default()),
            blocklist: Arc::new(blocklist),
            top_frame_seen: parking_lot::RwLock::new(Default::default()),
            fetch_registration: parking_lot::Mutex::new(None),
            entries_per_target: parking_lot::RwLock::new(Default::default()),
        });
        let interceptor = Arc::clone(&s);
        let handler: EventHandler = Arc::new(move |target_id, msg: CdpMessage| {
            // Document-only hashed path (status_code derivation, replay chain).
            if let Some(event) = parse_network_event(&msg.method, &msg.params) {
                interceptor.append(target_id, event);
            }
            // Full-capture observational path (xhr/fetch/subresource/document)
            // feeding `network_entries`. Separate accumulator — never touches
            // the hashed receipt.
            interceptor.observe_entry(target_id, &msg.method, &msg.params);
        });
        let registration = cdp.register_event_handler(EventFilter::new("Network."), handler);
        *s.registration.lock() = Some(registration);

        // Install the Fetch.* handler iff blocklist is non-empty.
        // Empty blocklist = no enforcement = no need to wire the gate.
        if !s.blocklist.is_empty() {
            let interceptor = Arc::clone(&s);
            let cdp_for_handler = cdp.clone();
            let fetch_handler: EventHandler = Arc::new(move |target_id, msg: CdpMessage| {
                if msg.method == "Fetch.requestPaused" {
                    interceptor.clone().handle_fetch_request_paused(
                        target_id,
                        msg.params,
                        cdp_for_handler.clone(),
                    );
                }
            });
            let fetch_reg = cdp.register_event_handler(EventFilter::new("Fetch."), fetch_handler);
            *s.fetch_registration.lock() = Some(fetch_reg);
        }
        s
    }

    /// Feed one `Network.*` event into the per-target full-capture accumulator
    /// (lazily created). Called from the `Network.*` handler closure for EVERY
    /// event; the accumulator itself filters to the 4 relevant methods.
    pub(crate) fn observe_entry(&self, target_id: TargetId, method: &str, params: &CborValue) {
        {
            let mut g = self.entries_per_target.write();
            g.entry(target_id)
                .or_insert_with(|| NetworkEntryAccumulator::with_cap(DEFAULT_MAX_NETWORK_ENTRIES));
        }
        let g = self.entries_per_target.read();
        if let Some(acc) = g.get(&target_id) {
            acc.observe(method, params);
        }
    }

    /// Inspect a `Fetch.requestPaused` event; either send
    /// `Fetch.continueRequest` (allowed) or `Fetch.failRequest`
    /// (blocked + recorded). Spawns a tokio task because the event
    /// handler closure is sync but `cdp.command()` is async.
    fn handle_fetch_request_paused(
        self: Arc<Self>,
        target_id: TargetId,
        params: CborValue,
        cdp: Arc<dyn CdpConnection>,
    ) {
        let CborValue::Map(map) = &params else {
            return;
        };
        let request_id = match cbor_map_text(map, "requestId") {
            Some(r) => r,
            None => return,
        };
        let url = cbor_map_text(map, "request")
            .or_else(|| {
                cbor_map_get(map, "request").and_then(|v| match v {
                    CborValue::Map(req_map) => cbor_map_text(req_map, "url"),
                    _ => None,
                })
            })
            .unwrap_or_default();
        let frame_id = cbor_map_text(map, "frameId").unwrap_or_default();
        let resource_type = cbor_map_text(map, "resourceType").unwrap_or_default();

        // The top-frame Document request is the operator's
        // primary URL — skip-gate regardless of blocklist match.
        // First Document event per (target_id, frame_id) is the
        // page-level navigate; subsequent same-frame Documents (e.g.
        // iframe navigations) ARE gated normally.
        let is_top_frame_doc = if resource_type == "Document" {
            let mut seen = self.top_frame_seen.write();
            let frames = seen.entry(target_id).or_default();
            frames.insert(frame_id.clone())
        } else {
            false
        };
        if is_top_frame_doc {
            spawn_continue(cdp, target_id, request_id);
            return;
        }

        match url_in_blocklist_strict(&url, &self.blocklist) {
            Some((category, pattern)) => {
                let blocked = BlockedEvent {
                    url: url.clone(),
                    reason: category.to_string(),
                    matched_pattern: pattern.to_string(),
                };
                self.blocked_per_target
                    .write()
                    .entry(target_id)
                    .or_default()
                    .push(blocked);
                tracing::info!(
                    target_id = target_id,
                    category = category,
                    pattern = pattern,
                    url = url.as_str(),
                    "blocklist: blocked sub-resource"
                );
                spawn_fail(cdp, target_id, request_id);
            }
            None => spawn_continue(cdp, target_id, request_id),
        }
    }
}

/// Spawn a `Fetch.continueRequest` (allowed pass-through). On error,
/// log; do NOT retry. The request fails closed at chromium's timeout.
fn spawn_continue(cdp: Arc<dyn CdpConnection>, target_id: TargetId, request_id: String) {
    tokio::spawn(async move {
        let msg = CdpMessage {
            method: "Fetch.continueRequest".into(),
            params: CborValue::Map(vec![(
                CborValue::Text("requestId".into()),
                CborValue::Text(request_id.clone()),
            )]),
        };
        if let Err(e) = cdp.command(target_id, msg, None).await {
            tracing::error!(
                target_id = target_id,
                request_id = request_id.as_str(),
                error = %e,
                "Fetch.continueRequest failed (request will fail closed at chromium timeout)"
            );
        }
    });
}

/// Spawn a `Fetch.failRequest{errorReason: "BlockedByClient"}`. On
/// error, log; the BlockedEvent has already been recorded so the
/// audit reflects intent even if the wire call fails.
fn spawn_fail(cdp: Arc<dyn CdpConnection>, target_id: TargetId, request_id: String) {
    tokio::spawn(async move {
        let msg = CdpMessage {
            method: "Fetch.failRequest".into(),
            params: CborValue::Map(vec![
                (
                    CborValue::Text("requestId".into()),
                    CborValue::Text(request_id.clone()),
                ),
                (
                    CborValue::Text("errorReason".into()),
                    CborValue::Text("BlockedByClient".into()),
                ),
            ]),
        };
        if let Err(e) = cdp.command(target_id, msg, None).await {
            tracing::warn!(
                target_id = target_id,
                request_id = request_id.as_str(),
                error = %e,
                "Fetch.failRequest failed; BlockedEvent already recorded so audit reflects intent"
            );
        }
    });
}

/// Public NetworkInterceptor trait surface.
#[async_trait::async_trait]
pub trait NetworkInterceptor: Send + Sync {
    /// Subscribe to interception for a target. Called by
    /// `ActionExecutor::page_navigate` BEFORE the navigate command,
    /// only when `blocklist_enabled` is true.
    ///
    /// Issues `Fetch.enable({patterns: [{urlPattern: "*", requestStage: "Request"}]})`
    /// (NOT the deprecated `Network.setRequestInterception`) via
    /// `CdpConnection`. With an empty blocklist the implementation
    /// short-circuits to `Ok(())` — see `new_with_blocklist`.
    async fn subscribe(&self, target_id: TargetId) -> Result<(), NetworkError>;

    /// Drain accumulated events for a target. Called by
    /// `ActionExecutor::page_navigate` after `Page.loadEventFired`.
    fn drain_events(&self, target_id: TargetId) -> Vec<LoomNetworkEvent>;

    /// Drain accumulated blocked sub-resource events for a target.
    /// Called by `ActionExecutor::page_navigate` after
    /// `Page.loadEventFired`; events become `AuditEntry::BlockedUrl`s
    /// in the manifest hash chain on the host side.
    fn drain_blocked(&self, target_id: TargetId) -> Vec<BlockedEvent>;

    /// Append an event. Used by tests + the chromiumoxide event
    /// handler closure. The closure does decompression-then-hash
    /// inside `compute_response_hash` before calling this.
    fn append(&self, target_id: TargetId, event: LoomNetworkEvent);

    /// State-invalidation cascade hook. Called by
    /// `Supervisor::handle_crash` indirectly (via TargetManager).
    fn clear_target(&self, target_id: TargetId);

    /// Read (NON-draining) the full-capture network-entries snapshot for a
    /// target, plus whether the shim-side cap truncated it. Used to populate
    /// the navigate receipt's `network_entries` and the `network_log` tool.
    /// Default returns empty (only `ChromiumNetworkInterceptor` accumulates).
    fn read_entries(&self, _target_id: TargetId) -> (Vec<LoomNetworkEntry>, bool) {
        (Vec::new(), false)
    }

    /// Clear the full-capture accumulator for a target. Called at navigate
    /// START so the navigate's `network_entries` reflect only that navigate;
    /// entries then accumulate across in-session actions until the next navigate.
    fn clear_entries(&self, _target_id: TargetId) {}
}

#[derive(Debug, thiserror::Error)]
pub enum NetworkError {
    #[error("CDP setRequestInterception failed: {0}")]
    SetInterceptionFailed(String),
    #[error("getResponseBody failed: {0}")]
    GetBodyFailed(String),
}

#[async_trait::async_trait]
impl NetworkInterceptor for ChromiumNetworkInterceptor {
    async fn subscribe(&self, target_id: TargetId) -> Result<(), NetworkError> {
        // Empty blocklist = no enforcement = no Fetch.enable. Avoids
        // the latency of issuing a no-op CDP command on every navigate
        // when the operator passed --no-blocklist OR no blocklist file
        // was loaded.
        if self.blocklist.is_empty() {
            return Ok(());
        }
        // Fetch.enable with `urlPattern: "*"` matches every request;
        // `requestStage: "Request"` fires before the request is sent
        // so we can decide continue/fail before the wire roundtrip.
        let msg = CdpMessage {
            method: "Fetch.enable".into(),
            params: CborValue::Map(vec![(
                CborValue::Text("patterns".into()),
                CborValue::Array(vec![CborValue::Map(vec![
                    (
                        CborValue::Text("urlPattern".into()),
                        CborValue::Text("*".into()),
                    ),
                    (
                        CborValue::Text("requestStage".into()),
                        CborValue::Text("Request".into()),
                    ),
                ])]),
            )]),
        };
        self.cdp
            .command(target_id, msg, None)
            .await
            .map_err(|e| NetworkError::SetInterceptionFailed(e.to_string()))?;
        Ok(())
    }

    fn drain_events(&self, target_id: TargetId) -> Vec<LoomNetworkEvent> {
        let mut g = self.per_target.write();
        g.remove(&target_id).unwrap_or_default()
    }

    fn drain_blocked(&self, target_id: TargetId) -> Vec<BlockedEvent> {
        let mut g = self.blocked_per_target.write();
        g.remove(&target_id).unwrap_or_default()
    }

    fn append(&self, target_id: TargetId, event: LoomNetworkEvent) {
        self.per_target
            .write()
            .entry(target_id)
            .or_default()
            .push(event);
    }

    fn clear_target(&self, target_id: TargetId) {
        self.per_target.write().remove(&target_id);
        self.blocked_per_target.write().remove(&target_id);
        self.top_frame_seen.write().remove(&target_id);
        self.entries_per_target.write().remove(&target_id);
    }

    fn read_entries(&self, target_id: TargetId) -> (Vec<LoomNetworkEntry>, bool) {
        match self.entries_per_target.read().get(&target_id) {
            Some(acc) => (acc.snapshot(), acc.truncated()),
            None => (Vec::new(), false),
        }
    }

    fn clear_entries(&self, target_id: TargetId) {
        if let Some(acc) = self.entries_per_target.read().get(&target_id) {
            acc.clear();
        }
    }
}

/// Parse a `Network.responseReceived` or `Network.loadingFailed` CDP
/// event into a `LoomNetworkEvent`. Returns `None` for events that
/// aren't main-document loads (subresource CSS/JS/images), so the
/// document-status derivation in `action_executor::page_navigate`
/// stays unambiguous. The shim is the only place
/// that classifies chromium error codes (D-01); the `error_kind` field
/// is set here for `loadingFailed`, mirroring the synthetic-event
/// path in `action_executor::page_navigate` for `Page.navigate`-time
/// transport failures.
pub fn parse_network_event(method: &str, params: &CborValue) -> Option<LoomNetworkEvent> {
    let map = match params {
        CborValue::Map(entries) => entries,
        _ => return None,
    };
    // Only main-document events feed the navigate receipt's status_code.
    // Real chromium emits subresource events with type="Stylesheet" /
    // "Script" / "Image"; we drop those. fake-chromium emits type="Document"
    // for the cases under test.
    if cbor_map_text(map, "type").is_none_or(|t| t != "Document") {
        return None;
    }
    match method {
        "Network.responseReceived" => {
            let response = match cbor_map_get(map, "response")? {
                CborValue::Map(r) => r,
                _ => return None,
            };
            let url = cbor_map_text(response, "url").unwrap_or_default();
            let status = cbor_map_u16(response, "status").unwrap_or(0);
            Some(LoomNetworkEvent {
                method: String::new(),
                url,
                request_hash: String::new(),
                response_hash: String::new(),
                status,
                content_type: cbor_map_text(response, "mimeType").unwrap_or_default(),
                duration_ms: 0,
                response_bytes: 0,
                error_reason: None,
                error_kind: None,
            })
        }
        "Network.loadingFailed" => {
            let error_text = cbor_map_text(map, "errorText").unwrap_or_default();
            if error_text.is_empty() {
                return None;
            }
            let kind = classify_chromium_nav_error(&error_text).to_string();
            Some(LoomNetworkEvent {
                method: String::new(),
                url: String::new(),
                request_hash: String::new(),
                response_hash: String::new(),
                status: 0,
                content_type: String::new(),
                duration_ms: 0,
                response_bytes: 0,
                error_reason: Some(error_text),
                error_kind: Some(kind),
            })
        }
        _ => None,
    }
}

fn cbor_map_get<'a>(map: &'a [(CborValue, CborValue)], key: &str) -> Option<&'a CborValue> {
    map.iter().find_map(|(k, v)| match k {
        CborValue::Text(s) if s == key => Some(v),
        _ => None,
    })
}

fn cbor_map_text(map: &[(CborValue, CborValue)], key: &str) -> Option<String> {
    match cbor_map_get(map, key)? {
        CborValue::Text(s) => Some(s.clone()),
        _ => None,
    }
}

fn cbor_map_u16(map: &[(CborValue, CborValue)], key: &str) -> Option<u16> {
    match cbor_map_get(map, key)? {
        CborValue::Integer(i) => u16::try_from(i128::from(*i)).ok(),
        _ => None,
    }
}

fn cbor_map_bool(map: &[(CborValue, CborValue)], key: &str) -> Option<bool> {
    match cbor_map_get(map, key)? {
        CborValue::Bool(b) => Some(*b),
        _ => None,
    }
}

/// CDP `wallTime` is a float (epoch seconds). Tolerate an integer encoding too.
fn cbor_map_f64(map: &[(CborValue, CborValue)], key: &str) -> Option<f64> {
    match cbor_map_get(map, key)? {
        CborValue::Float(f) => Some(*f),
        CborValue::Integer(i) => Some(i128::from(*i) as f64),
        _ => None,
    }
}

/// Pure helper: classify a chromium `Page.navigate` `errorText` string
/// into a stable `error_kind` value. The full chromium net-error name
/// list is in `net/base/net_error_list.h`; this function maps the
/// most common transport-layer failures into four classes. Anything
/// unrecognised falls through to `"network_error"` so receipt
/// consumers always see a typed kind (the raw chromium code stays in
/// `error_reason` for disambiguation).
///
/// Examples of recognised codes:
/// - `dns_failure`: `ERR_NAME_NOT_RESOLVED`, `ERR_NAME_RESOLUTION_FAILED`,
///   `ERR_DNS_*` (any DNS-prefix code)
/// - `connect_refused`: `ERR_CONNECTION_REFUSED`
/// - `tls_error`: `ERR_CERT_*` (e.g. `ERR_CERT_DATE_INVALID`,
///   `ERR_CERT_AUTHORITY_INVALID`, `ERR_CERT_COMMON_NAME_INVALID`),
///   `ERR_SSL_*` (e.g. `ERR_SSL_PROTOCOL_ERROR`,
///   `ERR_BAD_SSL_CLIENT_AUTH_CERT`)
/// - `blocked`: `ERR_BLOCKED_BY_CLIENT` (default analytics/ads/telemetry
///   blocklist hit) — distinct kind from `network_error` so agent
///   dashboards can match on intentional blocks vs. real connectivity
///   failures.
pub fn classify_chromium_nav_error(error_text: &str) -> &'static str {
    if error_text.contains("ERR_BLOCKED_BY_CLIENT")
        || error_text.contains("ERR_BLOCKED_BY_RESPONSE")
    {
        "blocked"
    } else if error_text.contains("ERR_NAME_NOT_RESOLVED")
        || error_text.contains("ERR_NAME_RESOLUTION_FAILED")
        || error_text.contains("ERR_DNS_")
    {
        "dns_failure"
    } else if error_text.contains("ERR_CONNECTION_REFUSED") {
        "connect_refused"
    } else if error_text.contains("ERR_CERT_") || error_text.contains("ERR_SSL_") {
        "tls_error"
    } else {
        "network_error"
    }
}

/// Pure helper: compute SHA-256 hex of decompressed bytes. R1 contract:
/// callers MUST pass already-decompressed bytes (chromiumoxide's
/// `Network.getResponseBody` returns those). Hashing pre-compressed
/// bytes here would still produce a valid hash but break replay
/// parity — the contract is documented to require decompressed input.
pub fn compute_response_hash(decompressed: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(decompressed);
    let bytes = h.finalize();
    hex_encode(&bytes)
}

/// Pure helper: strip `Content-Encoding` from a header list. The
/// canonical request descriptor is hashed AFTER this strip per R1
/// so request_hash is computed over the post-decompression
/// shape that the daemon will replay.
pub fn strip_content_encoding(headers: Vec<(String, String)>) -> Vec<(String, String)> {
    headers
        .into_iter()
        .filter(|(k, _)| !k.eq_ignore_ascii_case("content-encoding"))
        .collect()
}

/// Parse blocklist text into a `Vec<(category, pattern)>`. Tracks the
/// most recent `# --- Section ---` comment header and tags every
/// subsequent non-comment, non-blank line with the lowercased,
/// dash-and-space-trimmed section name. Lines preceding any section
/// header get `category="other"` (none in the current
/// `assets/default_blocklist.txt`). The input is typically the contents
/// of that file via `include_str!`.
///
/// Examples:
///   `# --- Analytics ---`     → category `"analytics"`
///   `# --- Advertising / Ad Networks ---` → `"advertising / ad networks"`
pub fn parse_blocklist_with_categories(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut current = String::from("other");
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('#') {
            let trimmed = rest
                .trim()
                .trim_matches(|c: char| c == '-' || c.is_whitespace());
            if !trimmed.is_empty() {
                current = trimmed.to_lowercase();
            }
            continue;
        }
        out.push((current.clone(), line.to_string()));
    }
    out
}

/// Pure helper: check whether a URL's HOST matches any pattern in the
/// categorized blocklist. Returns `Some((category, matched_pattern))`
/// on the first match, `None` otherwise.
///
/// Pattern semantics (host-only, NOT path/query):
/// - bare `domain` → URL host equals `domain`
/// - `*.suffix`    → URL host equals `suffix` OR ends with `.suffix`
///
/// URL parsing uses `url::Url`; URLs that don't parse OR don't have a
/// host return `None`. This is the production blocklist gate.
pub fn url_in_blocklist_strict<'a>(
    url: &str,
    patterns: &'a [(String, String)],
) -> Option<(&'a str, &'a str)> {
    let host = url::Url::parse(url).ok()?.host_str()?.to_lowercase();
    for (category, pattern) in patterns {
        let p = pattern.trim();
        if p.is_empty() || p.starts_with('#') {
            continue;
        }
        let matches = if let Some(suffix) = p.strip_prefix("*.") {
            let suffix_lc = suffix.to_lowercase();
            host == suffix_lc || host.ends_with(&format!(".{suffix_lc}"))
        } else {
            host == p.to_lowercase()
        };
        if matches {
            return Some((category.as_str(), pattern.as_str()));
        }
    }
    None
}

/// Lightweight hex encoder — avoids pulling another crate at the
/// interface header. May swap to `hex` crate later.
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}
