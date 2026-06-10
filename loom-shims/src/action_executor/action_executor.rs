// ActionExecutor — verb library + `cdp_send` worker.
//
// # Contract semantics
// - **R3 pre-condition.** Before dispatching any
//   `Page.navigate`, `ActionExecutor` calls
//   `TargetManager::determinism_ready(target_id)` and returns
//   `ShimErrorCode::ShimInternalError` (with detail
//   `"R3OrderingViolation"`) if the flag is still false.
// - **R1 pre-condition.** Before `Page.navigate`,
//   subscribes `NetworkInterceptor` for the target so the
//   `Network.responseReceived` events arrive before the response body
//   is evicted from Chromium's cache.
// - **`cdp_send` async.** Each `cdp_send` is its own
//   tokio task; multiple in-flight roundtrips do not serialise.
// - **No CDP payload escape.** `ActionExecutor`
//   translates CDP responses into typed `ActionResult` shapes.
//   `cdp_send` is the one path that round-trips opaque CBOR — the
//   daemon already routed that bytes-only via `loom-host::shim_call`,
//   never to a WASM caller.
// - **Per-target ordering for stateful actions.** Click → focus →
//   type sequence is enforced by serialising actions targeting the
//   same `target_id`; cross-target actions remain parallel.

use crate::cdp_connection::cdp_connection::{CdpConnection, CdpError, EventFilter, EventHandler};
use crate::dispatcher::dispatcher::make_error_response;
use crate::ipc_endpoint::ipc_endpoint::{CdpMessage, ShimErrorCode, ShimResponse, TargetId};
use crate::network_interceptor::network_interceptor::{
    classify_chromium_nav_error, BlockedEvent, LoomNetworkEntry, LoomNetworkEvent,
    NetworkInterceptor,
};
use crate::target_manager::target_manager::TargetManager;
use async_trait::async_trait;
use ciborium::value::Value as CborValue;
use loom_shared::navigate_outcome::ShimConsoleLine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;

/// Default per-action budget. Daemon may override per-call.
pub const DEFAULT_ACTION_BUDGET: Duration = Duration::from_secs(30);

/// Default budget specifically for `page_navigate`. Tighter than
/// `DEFAULT_ACTION_BUDGET` so an unreachable host (DNS failure,
/// connection refused, slow-TLS handshake) surfaces a typed-error
/// receipt within the network-error budget window. CDP itself fast-
/// fails DNS / connection-refused sub-second; this constant bounds
/// the slow-TLS / unresponsive-host worst case.
pub const DEFAULT_NAVIGATE_BUDGET: Duration = Duration::from_secs(10);

/// settle-capture: serde defaults for the readiness fields on
/// `ActionResult::Navigated`, so a pre-feature CBOR payload decodes unchanged.
fn default_settle_until_field() -> String {
    "settled".to_string()
}
fn default_settle_outcome_field() -> String {
    "reached".to_string()
}

/// Translated, typed result of an action. Never carries raw CDP bytes
/// outside `cdp_send`'s explicit pass-through path.
// Navigated carries dom_bytes + screenshot_bytes (heap Vec<u8>) alongside
// multiple Strings for the tier-2 receipt payload. All large fields are
// heap-allocated; the stack-frame size difference is pointer-width only.
// Boxing Navigated would add an extra allocation per navigate for no gain.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActionResult {
    /// `page_navigate` completed.
    Navigated {
        target_id: TargetId,
        frame_id: String,
        loader_id: String,
        network_events: Vec<LoomNetworkEvent>,
        /// SHA-256 of the captured DOM snapshot (post-load). Hex.
        dom_after_sha256: String,
        /// SHA-256 of the captured screenshot (post-load). Hex. NOT
        /// part of the bit-equal replay hash chain.
        screenshot_sha256: String,
        // --- Tier-2 payload fields ---
        /// Requested URL (from PageNavigate.url).
        url: String,
        /// Final URL after redirects. Stub: same as url.
        final_url: String,
        /// Page title. Stub: empty string.
        page_title: String,
        /// HTTP status of main document. Derived from network_events[0].status,
        /// falls back to 0 when network_events is empty.
        status_code: u16,
        /// Raw CBOR bytes of the DOM.getDocument response (dom_after_sha256 = sha256 of these).
        dom_bytes: Vec<u8>,
        /// Raw CBOR bytes of the Page.captureScreenshot response (screenshot_sha256 = sha256 of these).
        screenshot_bytes: Vec<u8>,
        /// Console lines captured by shim. Stub: always empty.
        console_lines: Vec<ShimConsoleLine>,
        /// Sub-resource requests blocked by the default blocklist.
        /// Drained from
        /// `NetworkInterceptor::drain_blocked` after `Page.loadEventFired`;
        /// the host writes one `AuditEntry { kind: BlockedUrl }` per
        /// event into the manifest hash chain. `serde(default)` so a
        /// pre-feature CBOR payload (no field) decodes as empty.
        #[serde(default)]
        blocked_events: Vec<BlockedEvent>,
        /// Raw per-request network entries (full-capture, observational —
        /// xhr/fetch/subresource/document) read from the `NetworkInterceptor`
        /// accumulator after `Page.loadEventFired`. NOT part of the replay
        /// hash chain. `serde(default)` so a pre-feature CBOR payload decodes
        /// with an empty vec.
        #[serde(default)]
        network_entries: Vec<LoomNetworkEntry>,
        /// True when the shim accumulator hit its cap and dropped entries.
        #[serde(default)]
        network_entries_truncated: bool,
        // --- settle-capture readiness fields ---
        /// Readiness mode the capture was gated on (`load|networkidle|settled`).
        #[serde(default = "default_settle_until_field")]
        settle_until: String,
        /// How the wait ended: `reached|timeout|dom_unstable`.
        #[serde(default = "default_settle_outcome_field")]
        settle_outcome: String,
        /// Virtual-tick count mapped to ms; diagnostic only (excluded from
        /// the host's outcome_hash).
        #[serde(default)]
        settle_ms: u64,
        /// In-flight (non-WS/SSE) request count at settle.
        #[serde(default)]
        network_count_at_settle: u64,
    },
    /// `cdp_send` round-trip; opaque pass-through to the daemon.
    CdpResult { result: CborValue },
    /// `page_close` completed.
    PageClosed { target_id: TargetId },
    /// settle-capture slice 2: `wait_for` completed — a standalone readiness
    /// wait on the current page (no navigation, no capture). Carries only the
    /// settle verdict; the host stamps `emitted_at_ms`.
    Waited {
        /// Readiness mode that was waited for (`load|networkidle|settled`).
        settle_until: String,
        /// How the wait ended: `reached|timeout|dom_unstable`.
        settle_outcome: String,
        /// Virtual-tick count mapped to ms; diagnostic only (excluded from the
        /// host's outcome_hash).
        settle_ms: u64,
        /// In-flight (non-WS/SSE) request count at settle.
        network_count_at_settle: u64,
    },
}

/// Concrete ActionExecutor.
pub struct ChromiumActionExecutor {
    pub(crate) cdp: Arc<dyn CdpConnection>,
    pub(crate) network: Arc<dyn NetworkInterceptor>,
    pub(crate) targets: Arc<dyn TargetManager>,
    pub(crate) default_budget: Duration,
}

impl ChromiumActionExecutor {
    pub fn new(
        cdp: Arc<dyn CdpConnection>,
        network: Arc<dyn NetworkInterceptor>,
        targets: Arc<dyn TargetManager>,
    ) -> Self {
        Self {
            cdp,
            network,
            targets,
            default_budget: DEFAULT_ACTION_BUDGET,
        }
    }
}

/// Public ActionExecutor trait surface. All methods are async so the
/// dispatcher can drive multiple in-flight CDP roundtrips concurrently
/// and so that L4's `chromiumoxide::Browser` calls — which
/// are inherently async — fit cleanly into the call chain.
#[async_trait]
pub trait ActionExecutor: Send + Sync {
    /// Pass-through CDP command. p99 ≤ 50ms.
    /// Errors: `CdpTimeout`, `CdpProtocolError`, `TargetUnknown`.
    async fn cdp_send(
        &self,
        target_id: TargetId,
        msg: CdpMessage,
        budget: Option<Duration>,
    ) -> Result<ActionResult, ShimResponse>;

    /// Navigate target to URL. R3 pre-check + R1 subscription before
    /// `Page.navigate`. Drains `LoomNetworkEvent`s from `NetworkInterceptor`
    /// after `Page.loadEventFired`.
    ///
    /// `blocklist_enabled` — when true,
    /// also issues `Fetch.enable` before navigate so sub-resources are
    /// gated against the default blocklist; drained `BlockedEvent`s
    /// land in the receipt's `blocked_events` field and become
    /// manifest `AuditEntry { kind: BlockedUrl }` on the host side.
    /// When false, no `Fetch.enable` is sent and `blocked_events` is
    /// empty.
    ///
    /// Errors: `R3OrderingViolation` if `determinism_injected == false`;
    /// `CdpTimeout` if budget exceeded.
    async fn page_navigate(
        &self,
        target_id: TargetId,
        url: String,
        budget: Option<Duration>,
        blocklist_enabled: bool,
        // settle-capture: readiness state the capture is gated on.
        settle_mode: crate::readiness_monitor::SettleMode,
    ) -> Result<ActionResult, ShimResponse>;

    /// settle-capture slice 2: run a standalone readiness wait on `target_id`
    /// (no navigation, no capture), reusing the SettleDriver/ReadinessMonitor.
    /// Returns `ActionResult::Waited` with the settle verdict. Bounded by the
    /// tick ceiling derived from `budget` — returns a typed `timeout` /
    /// `dom_unstable` verdict rather than hanging.
    async fn wait_for(
        &self,
        target_id: TargetId,
        settle_mode: crate::readiness_monitor::SettleMode,
        budget: Option<Duration>,
    ) -> Result<ActionResult, ShimResponse>;

    /// Close the target.
    async fn page_close(&self, target_id: TargetId) -> Result<ActionResult, ShimResponse>;

    /// Read (NON-draining) the full-capture network-entries snapshot for the
    /// target — everything observed since the last navigate. Backs the
    /// `loom.web.network_log` tool. Returns `(entries, shim_truncated)`.
    /// Default returns empty (only `ChromiumActionExecutor` accumulates).
    fn read_network_log(&self, _target_id: TargetId) -> (Vec<LoomNetworkEntry>, bool) {
        (Vec::new(), false)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ActionError {
    #[error("R3 ordering violation: navigate before determinism inject on target {0}")]
    R3OrderingViolation(TargetId),
    #[error("CDP failure: {0}")]
    Cdp(#[from] CdpError),
    #[error("target {0} unknown")]
    TargetUnknown(TargetId),
}

impl From<ActionError> for ShimErrorCode {
    fn from(e: ActionError) -> Self {
        match e {
            ActionError::R3OrderingViolation(_) => ShimErrorCode::ShimInternalError,
            ActionError::Cdp(c) => c.into(),
            ActionError::TargetUnknown(_) => ShimErrorCode::TargetUnknown,
        }
    }
}

#[async_trait]
impl ActionExecutor for ChromiumActionExecutor {
    async fn cdp_send(
        &self,
        target_id: TargetId,
        msg: CdpMessage,
        budget: Option<Duration>,
    ) -> Result<ActionResult, ShimResponse> {
        // Snapshot (and the other guest verbs) reach DOM.getDocument via
        // this generic path and hash the raw response; normalize it here too so
        // their `dom_snapshot_hash` is content-stable, just like navigate STEP 5.
        let is_dom_get_document = msg.method == "DOM.getDocument";
        match self.cdp.command(target_id, msg, budget).await {
            Ok(result) => {
                let result = if is_dom_get_document {
                    let normalized =
                        loom_shared::dom_normalize::normalize_dom_cbor(&cbor_to_bytes(&result));
                    ciborium::de::from_reader(normalized.as_bytes()).unwrap_or(result)
                } else {
                    result
                };
                Ok(ActionResult::CdpResult { result })
            }
            Err(e) => Err(action_error_to_response(ActionError::Cdp(e), 0, None)),
        }
    }

    async fn page_navigate(
        &self,
        target_id: TargetId,
        url: String,
        budget: Option<Duration>,
        blocklist_enabled: bool,
        settle_mode: crate::readiness_monitor::SettleMode,
    ) -> Result<ActionResult, ShimResponse> {
        // R3 PRECONDITION. Refuse to navigate before the
        // determinism script has been installed for this target. Today
        // the flag flips true ONLY on Ok(()) from `inject` (per
        // target_manager); a false flag here would mean a regression
        // re-introduced the silent-success path. Defense-in-depth.
        if !self.targets.determinism_ready(target_id) {
            tracing::error!(target_id, "R3OrderingViolation: navigate before inject");
            return Err(make_error_response(
                0,
                None,
                ShimErrorCode::ShimInternalError,
                "R3OrderingViolation",
            ));
        }

        // Tighter default for navigate so unreachable hosts surface a
        // typed-error receipt within the network-error budget window. Callers
        // passing an explicit `budget` keep their value.
        let timeout = budget.unwrap_or(DEFAULT_NAVIGATE_BUDGET);

        // STEP 1: subscribe to Page.loadEventFired BEFORE issuing navigate
        // (per practitioner bug magnet #1: cached / data: URLs fire fast,
        // and post-navigate subscription will miss the event).
        let load_signal: Arc<parking_lot::Mutex<Option<oneshot::Sender<()>>>> =
            Arc::new(parking_lot::Mutex::new(None));
        let (load_tx, load_rx) = oneshot::channel::<()>();
        *load_signal.lock() = Some(load_tx);
        let signal_for_handler = load_signal.clone();
        let load_handler: EventHandler = Arc::new(move |_target_id, msg: CdpMessage| {
            if msg.method == "Page.loadEventFired" {
                if let Some(tx) = signal_for_handler.lock().take() {
                    let _ = tx.send(());
                }
            }
        });
        let _load_reg = self
            .cdp
            .register_event_handler(EventFilter::new("Page.loadEventFired"), load_handler);

        // Reset the full-capture network-entries accumulator at navigate START
        // so this navigate's `network_entries` reflect only this navigate;
        // entries then accumulate across in-session clicks/evaluate until the
        // next navigate. CDP events arrive under target_id==0 (see the drain
        // note below); clear both keys to cover the fake-chromium per-target path.
        self.network.clear_entries(0);
        self.network.clear_entries(target_id);

        // STEP 1b: subscribe to Runtime.consoleAPICalled to accumulate
        // console output during the navigate.. The
        // collector is dropped at the end of this fn so each navigate
        // gets a fresh log; cross-action persistence is not part of the
        // brief.
        let console_collector: Arc<parking_lot::Mutex<Vec<ShimConsoleLine>>> =
            Arc::new(parking_lot::Mutex::new(Vec::new()));
        let console_for_handler = console_collector.clone();
        let console_handler: EventHandler = Arc::new(move |_target_id, msg: CdpMessage| {
            if msg.method == "Runtime.consoleAPICalled" {
                if let Some(line) = extract_console_line(&msg.params) {
                    console_for_handler.lock().push(line);
                }
            }
        });
        let _console_reg = self.cdp.register_event_handler(
            EventFilter::new("Runtime.consoleAPICalled"),
            console_handler,
        );

        // STEP 2a: subscribe to Network.responseReceived to capture network events.
        // Stub: NetworkInterceptor handles its own event subscription at boot,
        // so we just snapshot the events accumulated so far at navigate end.

        // STEP 2b: subscribe to Fetch.requestPaused for sub-resource
        // blocklist enforcement. Skipped
        // when the operator passed --no-blocklist (decoded into
        // `blocklist_enabled = false` on the wire). The interceptor's
        // own constructor short-circuits when its blocklist is empty,
        // so the cost of subscribing per-navigate is just one
        // `Fetch.enable` CDP roundtrip when enabled.
        if blocklist_enabled {
            if let Err(e) = self.network.subscribe(target_id).await {
                tracing::warn!(target_id, error = %e, "blocklist Fetch.enable failed; sub-resources will not be gated for this navigate");
            }
        }

        // STEP 3: send Page.navigate.
        let mut nav_params = vec![
            (CborValue::Text("url".into()), CborValue::Text(url.clone())),
            (
                CborValue::Text("transitionType".into()),
                CborValue::Text("typed".into()),
            ),
        ];
        // Drop the borrow checker shenanigans: just construct directly.
        let nav_msg = CdpMessage {
            method: "Page.navigate".into(),
            params: CborValue::Map(std::mem::take(&mut nav_params)),
        };
        let nav_response = self
            .cdp
            .command(target_id, nav_msg, Some(timeout))
            .await
            .map_err(|e| action_error_to_response(ActionError::Cdp(e), 0, None))?;
        let (frame_id, loader_id) = extract_frame_loader(&nav_response);

        // STEP 3b (faithful-entrance-animations): grant a bounded virtual-time
        // budget for THIS navigation so the page's entrance animations run to
        // completion while the clock stays deterministic. The budget is a hard
        // ceiling (security boundary) so a never-terminating animation pauses
        // instead of spinning the renderer unbounded. Origin was pinned at
        // inject; we only re-arm the budget here. Best-effort: a failure leaves
        // the page on whatever clock the inject step established.
        if crate::determinism_injector::determinism_injector::virtual_time_enabled() {
            let vt_budget = CdpMessage {
                method: crate::determinism_injector::determinism_injector::VIRTUAL_TIME_METHOD
                    .to_string(),
                params:
                    crate::determinism_injector::determinism_injector::build_virtual_time_budget_params(
                    ),
            };
            if let Err(e) = self.cdp.command(target_id, vt_budget, Some(timeout)).await {
                tracing::warn!(target_id, error = %e, "navigate: virtual-time budget re-arm failed");
            }
        }

        // STEP 4: await Page.loadEventFired with timeout. The fake-chromium
        // harness emits the event right after Page.navigate response so this
        // typically completes immediately. Real Chromium can take longer.
        let load_fired = tokio::time::timeout(timeout, load_rx).await.is_ok();

        // STEP 4b (settle-capture): gate the capture on the requested readiness
        // state. The verdict is a pure function of the per-tick observation
        // sequence in virtual ticks (DET-CORE), so it is replay-equal. `load`
        // mode returns immediately once load fired; networkidle/settled poll
        // until quiet or the tick ceiling (a bounded, typed fallback).
        let settle = crate::readiness_monitor::wait_for_settle(
            &self.cdp,
            target_id,
            settle_mode,
            crate::readiness_monitor::settle_driver::config_for_timeout(timeout),
            load_fired,
            timeout,
        )
        .await;

        // STEP 5: DOM.getDocument → raw CBOR bytes + SHA-256.
        let dom_msg = CdpMessage {
            method: "DOM.getDocument".into(),
            params: CborValue::Map(vec![
                (
                    CborValue::Text("depth".into()),
                    CborValue::Integer((-1i64).into()),
                ),
                (CborValue::Text("pierce".into()), CborValue::Bool(true)),
            ]),
        };
        let dom_result = self
            .cdp
            .command(target_id, dom_msg, Some(timeout))
            .await
            .map_err(|e| action_error_to_response(ActionError::Cdp(e), 0, None))?;
        // Strip the ephemeral per-navigation `frameId` (and any future ephemeral
        // CDP id) from the DOM CBOR before it is hashed or stored, so two
        // independent same-seed captures of byte-identical content produce the
        // same `dom_snapshot_hash`. The host re-hashes these stored bytes, so the
        // bytes themselves (not just the side hash) must be normalized.
        let dom_bytes = loom_shared::dom_normalize::normalize_dom_cbor(&cbor_to_bytes(&dom_result))
            .into_bytes();
        let dom_after_sha256 = sha256_hex_of_bytes(&dom_bytes);

        // STEP 6: Page.captureScreenshot → raw CBOR bytes + SHA-256.
        let shot_msg = CdpMessage {
            method: "Page.captureScreenshot".into(),
            params: CborValue::Map(vec![(
                CborValue::Text("format".into()),
                CborValue::Text("png".into()),
            )]),
        };
        let shot_result = self
            .cdp
            .command(target_id, shot_msg, Some(timeout))
            .await
            .map_err(|e| action_error_to_response(ActionError::Cdp(e), 0, None))?;
        // Decode the CDP screenshot envelope (CBOR `{data: <base64-PNG>}`)
        // into raw PNG bytes so the content store holds a renderable image
        // rather than a double-encoded envelope. On the rare decode failure
        // fall back to the raw CBOR bytes (pre-fix behaviour) so navigate
        // never breaks on an unexpected response shape.
        let raw_cbor = cbor_to_bytes(&shot_result);
        let screenshot_bytes = match loom_shared::screenshot_decode::decode_cdp_screenshot(
            &raw_cbor,
        ) {
            Ok(png) => png,
            Err(e) => {
                tracing::warn!(error = %e, "screenshot decode failed; storing raw CDP envelope");
                raw_cbor
            }
        };
        let screenshot_sha256 = sha256_hex_of_bytes(&screenshot_bytes);

        // STEP 6a: extract document.title and final_url via a single
        // Runtime.evaluate roundtrip. This replaces the earlier stubs
        // that left both fields empty. Both
        // are best-effort: a CDP error here doesn't fail the navigate
        // — we just leave the field empty (same as the stub it
        // replaces). Single roundtrip keeps the cost ~constant vs
        // separate calls.
        let (page_title, real_final_url) = {
            let eval_msg = CdpMessage {
                method: "Runtime.evaluate".into(),
                params: CborValue::Map(vec![
                    (
                        CborValue::Text("expression".into()),
                        CborValue::Text(
                            "JSON.stringify([document.title || '', \
                              location.href || ''])"
                                .into(),
                        ),
                    ),
                    (
                        CborValue::Text("returnByValue".into()),
                        CborValue::Bool(true),
                    ),
                    (
                        CborValue::Text("awaitPromise".into()),
                        CborValue::Bool(false),
                    ),
                ]),
            };
            match self.cdp.command(target_id, eval_msg, Some(timeout)).await {
                Ok(r) => extract_title_and_url_from_evaluate(&r)
                    .unwrap_or_else(|| (String::new(), url.clone())),
                Err(_) => (String::new(), url.clone()),
            }
        };

        // STEP 7: drain network events + blocked sub-resource events
        // accumulated by NetworkInterceptor. Blocked events become
        // manifest `AuditEntry { kind: BlockedUrl }` on the host side.
        //
        // CDP envelope events arrive at handlers with `target_id == 0`
        // (cdp_connection's read_loop hardcodes that — there's no
        // multi-target routing today; one chromium subprocess per
        // session, one CDP session). NetworkInterceptor::append stores
        // events under `target_id == 0`, so we drain from there too.
        // Falling back to the requested `target_id` keeps the
        // fake-chromium harness's per-target drain working — that
        // path uses real per-target IDs.
        let mut network_events = self.network.drain_events(0);
        if network_events.is_empty() {
            network_events = self.network.drain_events(target_id);
        }
        let mut blocked_events = self.network.drain_blocked(0);
        if blocked_events.is_empty() {
            blocked_events = self.network.drain_blocked(target_id);
        }

        // Read (NON-draining) the full-capture network-entries snapshot — the
        // accumulator persists across in-session actions for the `network_log`
        // tool. Mirror the target_id==0 / target_id duality used above.
        let (network_entries, network_entries_truncated) = {
            let from_zero = self.network.read_entries(0);
            if from_zero.0.is_empty() {
                self.network.read_entries(target_id)
            } else {
                from_zero
            }
        };

        // Derive status_code from first network event; fall back to 0.
        // Computed BEFORE any synthetic-event push so HTTP status from a
        // real Network.responseReceived isn't shadowed by a status=0
        // synthetic event we appended for DNS-failure signaling.
        let status_code = network_events.first().map(|e| e.status).unwrap_or(0);

        // Surface DNS / network failures via a synthetic LoomNetworkEvent
        // carrying `error_reason`. CDP `Page.navigate` returns a non-empty
        // `errorText` field when navigation could not even reach a server
        // (DNS, conn-refused, TLS, etc.). The host-side `navigate_execute`
        // detects events with `error_reason.is_some()` and converts them
        // into an HostError::ShimFailure carrying a structured JSON
        // detail (kind="dns_failure").
        if let Some(err_text) = extract_nav_error_text(&nav_response) {
            // Classify at the boundary (D-01): the shim owns chromium-
            // specific error mapping, the host reads the typed kind.
            let kind = classify_chromium_nav_error(&err_text).to_string();
            network_events.push(LoomNetworkEvent {
                method: String::new(),
                url: url.clone(),
                request_hash: String::new(),
                response_hash: String::new(),
                status: 0,
                content_type: String::new(),
                duration_ms: 0,
                response_bytes: 0,
                error_reason: Some(err_text),
                error_kind: Some(kind),
            });
        }

        Ok(ActionResult::Navigated {
            target_id,
            frame_id,
            loader_id,
            network_events,
            dom_after_sha256,
            screenshot_sha256,
            url: url.clone(),
            // real final_url from `location.href` (post-
            // redirect; falls back to requested URL on CDP error).
            final_url: real_final_url,
            // real page_title from `document.title`
            // (empty string is a legitimate value when the page has no
            // <title> — distinguishable from the prior unconditional stub
            // because final_url is no longer == url for redirecting pages).
            page_title,
            status_code,
            dom_bytes,
            screenshot_bytes,
            // console_lines populated from the
            // Runtime.consoleAPICalled events that arrived during this
            // navigate. The collector was subscribed BEFORE Page.navigate
            // so messages fired during page load (cached / data: URLs
            // included) are captured.
            console_lines: {
                let mut guard = console_collector.lock();
                std::mem::take(&mut *guard)
            },
            settle_until: settle.mode.as_str().to_string(),
            settle_outcome: settle.outcome.as_str().to_string(),
            // Map the virtual tick count to a ms diagnostic via the pacing
            // cadence. Excluded from outcome_hash host-side.
            settle_ms: settle.ticks as u64 * 5,
            network_count_at_settle: settle.network_count as u64,
            blocked_events,
            network_entries,
            network_entries_truncated,
        })
    }

    async fn wait_for(
        &self,
        target_id: TargetId,
        settle_mode: crate::readiness_monitor::SettleMode,
        budget: Option<Duration>,
    ) -> Result<ActionResult, ShimResponse> {
        // Standalone readiness wait on an ALREADY-LOADED page. Unlike navigate
        // we did not subscribe to (and observe) Page.loadEventFired for this
        // call — the page has already loaded by the time a caller invokes
        // wait_for — so `load_fired` is true. The verdict is a pure function of
        // the per-tick observation sequence in virtual ticks (DET-CORE), so it
        // replays identically; the tick ceiling (from `budget`) bounds it to a
        // typed `timeout`/`dom_unstable` instead of hanging.
        let timeout = budget.unwrap_or(DEFAULT_NAVIGATE_BUDGET);
        let settle = crate::readiness_monitor::wait_for_settle(
            &self.cdp,
            target_id,
            settle_mode,
            crate::readiness_monitor::settle_driver::config_for_timeout(timeout),
            true,
            timeout,
        )
        .await;

        Ok(ActionResult::Waited {
            settle_until: settle.mode.as_str().to_string(),
            settle_outcome: settle.outcome.as_str().to_string(),
            settle_ms: settle.ticks as u64 * 5,
            network_count_at_settle: settle.network_count as u64,
        })
    }

    async fn page_close(&self, target_id: TargetId) -> Result<ActionResult, ShimResponse> {
        // Target.closeTarget over CDP. Best-effort — even if the CDP call
        // fails (Chromium already shut down), the upstream caller treats
        // the target as gone.
        let close_msg = CdpMessage {
            method: "Target.closeTarget".into(),
            params: CborValue::Map(vec![(
                CborValue::Text("targetId".into()),
                CborValue::Text(format!("{target_id}")),
            )]),
        };
        let _ = self.cdp.command(target_id, close_msg, None).await;
        Ok(ActionResult::PageClosed { target_id })
    }

    fn read_network_log(&self, target_id: TargetId) -> (Vec<LoomNetworkEntry>, bool) {
        // CDP events accumulate under target_id==0 (see page_navigate drain
        // note); fall back to the explicit target for the fake-chromium
        // per-target path.
        let from_zero = self.network.read_entries(0);
        if from_zero.0.is_empty() {
            self.network.read_entries(target_id)
        } else {
            from_zero
        }
    }
}

/// Pull a non-empty `errorText` field from a `Page.navigate` CBOR
/// response Map. CDP populates this when navigation fails before a
/// server response (DNS, connection refused, TLS, etc.). Returns
/// `None` if the field is absent or empty (success path).
pub(super) fn extract_nav_error_text(response: &CborValue) -> Option<String> {
    if let CborValue::Map(entries) = response {
        for (k, v) in entries {
            if let (CborValue::Text(key), CborValue::Text(val)) = (k, v) {
                if key == "errorText" && !val.is_empty() {
                    return Some(val.clone());
                }
            }
        }
    }
    None
}

/// Pull `(title, location.href)` out of a `Runtime.evaluate` CBOR
/// response whose `expression` was
/// `JSON.stringify([document.title || '', location.href || ''])`.
///
/// The CDP response shape is:
///   { result: { type: "string", value: "[\"title\",\"url\"]" } }
///
/// Returns `None` when the response doesn't match the expected shape
/// (e.g. CDP error, page hadn't finished load yet, or guard was
/// triggered) — caller falls back to the requested URL + empty title.
pub(super) fn extract_title_and_url_from_evaluate(
    response: &CborValue,
) -> Option<(String, String)> {
    let map = match response {
        CborValue::Map(m) => m,
        _ => return None,
    };
    // Walk to result.value (string carrying the JSON-encoded [title,url] array).
    let result_value = map.iter().find_map(|(k, v)| match k {
        CborValue::Text(s) if s == "result" => Some(v),
        _ => None,
    })?;
    let result_map = match result_value {
        CborValue::Map(m) => m,
        _ => return None,
    };
    let value = result_map.iter().find_map(|(k, v)| match k {
        CborValue::Text(s) if s == "value" => Some(v),
        _ => None,
    })?;
    let json_str = match value {
        CborValue::Text(s) => s,
        _ => return None,
    };
    let parsed: serde_json::Value = serde_json::from_str(json_str).ok()?;
    let arr = parsed.as_array()?;
    if arr.len() != 2 {
        return None;
    }
    let title = arr[0].as_str()?.to_string();
    let final_url = arr[1].as_str()?.to_string();
    Some((title, final_url))
}

/// Extract a `ShimConsoleLine` from a `Runtime.consoleAPICalled` event
/// params. CDP shape:
///   { type: "log"|"warn"|"error"|"info"|..., args: [{type,value}, ...], ... }
///
/// Multi-arg console.log (e.g. `console.log("count:", 42)`) is joined with
/// a single space — matches Chromium devtools' rendering. Non-string
/// args are stringified via the value field as a best-effort (rich
/// inspection is not part of the brief; receipts target agent
/// consumption, not human debugging UX).
pub(super) fn extract_console_line(params: &CborValue) -> Option<ShimConsoleLine> {
    let map = match params {
        CborValue::Map(m) => m,
        _ => return None,
    };
    let level = map
        .iter()
        .find_map(|(k, v)| match (k, v) {
            (CborValue::Text(s), CborValue::Text(val)) if s == "type" => Some(val.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "log".to_string());
    let args = map.iter().find_map(|(k, v)| match k {
        CborValue::Text(s) if s == "args" => Some(v),
        _ => None,
    })?;
    let args_arr = match args {
        CborValue::Array(a) => a,
        _ => return None,
    };
    let parts: Vec<String> = args_arr
        .iter()
        .filter_map(|arg| match arg {
            CborValue::Map(m) => m.iter().find_map(|(k, v)| match (k, v) {
                (CborValue::Text(s), CborValue::Text(val)) if s == "value" => Some(val.clone()),
                (CborValue::Text(s), CborValue::Integer(i)) if s == "value" => {
                    Some(i128::from(*i).to_string())
                }
                (CborValue::Text(s), CborValue::Bool(b)) if s == "value" => Some(b.to_string()),
                _ => None,
            }),
            _ => None,
        })
        .collect();
    let message = if parts.is_empty() {
        return None;
    } else {
        parts.join(" ")
    };
    Some(ShimConsoleLine { level, message })
}

/// Pull `frameId` + `loaderId` from a `Page.navigate` CBOR response Map.
fn extract_frame_loader(response: &CborValue) -> (String, String) {
    let mut frame_id = String::new();
    let mut loader_id = String::new();
    if let CborValue::Map(entries) = response {
        for (k, v) in entries {
            if let (CborValue::Text(key), CborValue::Text(val)) = (k, v) {
                match key.as_str() {
                    "frameId" => frame_id = val.clone(),
                    "loaderId" => loader_id = val.clone(),
                    _ => {}
                }
            }
        }
    }
    (frame_id, loader_id)
}

/// Serialize a CBOR value to raw bytes. Used to populate dom_bytes / screenshot_bytes.
fn cbor_to_bytes(value: &CborValue) -> Vec<u8> {
    let mut bytes = Vec::new();
    let _ = ciborium::ser::into_writer(value, &mut bytes);
    bytes
}

/// Compute SHA-256 (lowercase hex) of raw bytes. Used for `screenshot_sha256`
/// now that the screenshot is stored as a decoded PNG rather than its CBOR
/// envelope.
fn sha256_hex_of_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for b in digest.iter() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Pure helper: convert an `ActionError` into the typed
/// `ShimResponse::Error` envelope. Centralised so all error returns
/// have consistent detail strings. `request_id` is supplied by the
/// caller (typically the dispatcher echoing the originating request id).
pub fn action_error_to_response(
    err: ActionError,
    request_id: u64,
    session_id: Option<u64>,
) -> ShimResponse {
    let detail = err.to_string();
    let code: ShimErrorCode = err.into();
    ShimResponse::Error {
        request_id,
        session_id,
        code,
        detail,
    }
}
