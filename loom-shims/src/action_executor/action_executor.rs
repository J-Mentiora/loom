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

use crate::cdp_connection::cdp_connection::{
    CdpConnection, CdpError, EventFilter, EventHandler, EventRegistration,
};
use crate::dispatcher::dispatcher::make_error_response;
use crate::ipc_endpoint::ipc_endpoint::{CdpMessage, ShimErrorCode, ShimResponse, TargetId};
use crate::network_interceptor::network_interceptor::{
    classify_chromium_nav_error, BlockedEvent, EventAttribution, LoomNetworkEntry,
    LoomNetworkEvent, NetworkInterceptor,
};
use crate::target_manager::target_manager::TargetManager;
use async_trait::async_trait;
use ciborium::value::Value as CborValue;
use loom_shared::navigate_outcome::ShimConsoleLine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::sync::OnceLock;
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
///
/// Overridable at process start via `LOOM_SHIM_CDP_TIMEOUT_MS` (see
/// [`navigate_budget`]) — production topologies running many orchestrator
/// instances can raise it so a slow-but-healthy navigate on a CPU-saturated
/// host isn't trapped as a timeout. The default is unchanged.
pub const DEFAULT_NAVIGATE_BUDGET: Duration = Duration::from_secs(10);

/// Env var (milliseconds) overriding the per-CDP-command navigate budget.
const NAVIGATE_BUDGET_ENV: &str = "LOOM_SHIM_CDP_TIMEOUT_MS";

/// Upper bound for the best-effort virtual-time RESUME on a navigate exit
/// (animation-capture Mode A). The resume is a single `setVirtualTimePolicy`
/// round-trip, so it should return sub-second on a healthy renderer; capping it
/// here keeps a failed navigate's cleanup from inheriting the full navigate budget
/// (which can be 10s+) and guarantees the cleanup can never itself wedge.
const RESUME_CDP_TIMEOUT: Duration = Duration::from_secs(2);

/// Maximum number of unsolicited top-level navigations (client-side redirects)
/// the settle path will FOLLOW within a single navigate/wait_for before giving
/// up and returning the bounded `timeout` outcome (client-nav-reattach D4). A
/// page that bounces between login states forever (a redirect loop) is capped
/// here so the re-attach loop can never spin unbounded; the per-action wall-clock
/// deadline is the other, tighter bound. 10 covers real multi-step auth flows
/// (SPA shell → IdP → consent → app) with headroom.
const MAX_REATTACH_HOPS: u32 = 10;

/// Which command is driving the shared client-nav re-attach loop. Selects the
/// log verb and the exhausted-chain tail so `page_navigate` and `wait_for`
/// stay distinguishable in traces (the messages are otherwise identical).
#[derive(Clone, Copy)]
enum ReattachKind {
    Navigate,
    WaitFor,
}

impl ReattachKind {
    fn verb(self) -> &'static str {
        match self {
            ReattachKind::Navigate => "navigate",
            ReattachKind::WaitFor => "wait_for",
        }
    }

    /// The clause appended to the hop-cap/deadline-exhausted warning — navigate
    /// captures the current document, wait_for resolves on it.
    fn exhausted_tail(self) -> &'static str {
        match self {
            ReattachKind::Navigate => "capturing current document",
            ReattachKind::WaitFor => "resolving on current document",
        }
    }
}

/// Result of driving the bounded client-nav re-attach chain shared by
/// `page_navigate` and `wait_for`. `settle` is the final settle verdict to
/// capture/report on; `last_drained` is `Some(budget_drained)` from the final
/// determinism-clock re-arm hop, or `None` when no vt-arm hop ran (no
/// renavigation, or virtual time disabled) — letting each caller apply its own
/// post-loop virtual-time bookkeeping without the helper knowing about it.
struct ReattachOutcome {
    settle: crate::readiness_monitor::SettleResult,
    last_drained: Option<bool>,
}

/// Parse a `LOOM_SHIM_CDP_TIMEOUT_MS` value into a navigate budget. A missing,
/// non-numeric, or non-positive value falls back to [`DEFAULT_NAVIGATE_BUDGET`]
/// (a `0`/garbage knob must not disable the budget — that would re-introduce
/// the unbounded-navigate trap this guards against). Pure so the parse policy
/// is unit-testable without the process-global env or the cache below.
pub(crate) fn parse_navigate_budget(raw: Option<&str>) -> Duration {
    raw.and_then(|s| s.parse::<u64>().ok())
        .filter(|&ms| ms > 0)
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_NAVIGATE_BUDGET)
}

/// Per-CDP-command navigate budget, read from `LOOM_SHIM_CDP_TIMEOUT_MS` once
/// per process and cached (mirrors `connection_handler::max_concurrent_requests`).
/// Used only when the caller passes no explicit `budget` — which the dispatcher
/// always does for `Page.navigate`, so this is the binding timeout in production.
fn navigate_budget() -> Duration {
    static CACHED: OnceLock<Duration> = OnceLock::new();
    *CACHED
        .get_or_init(|| parse_navigate_budget(std::env::var(NAVIGATE_BUDGET_ENV).ok().as_deref()))
}

/// Decide whether to RESUME (advance) the renderer's virtual clock on navigate
/// exit. The clock is left FROZEN at the drained budget horizon ONLY on the
/// determinism-pinned clean-drain path (`clock_pinned && budget_drained`), where
/// a later `web.evaluate` clock read must replay byte-equal. In EVERY other case
/// — determinism OFF (`clock_pinned == false`), or the budget did not cleanly
/// drain — the clock must be resumed: a paused virtual clock defers the NEXT
/// navigate's `Page.loadEventFired`, and the determinism-OFF navigate path awaits
/// load BEFORE re-arming a budget, so it would deadlock on the deferred load and
/// burn the full navigate budget every call (navigate-degradation regression;
/// the +20s-per-navigate wedge). `vt_active == false` means virtual time is
/// disabled outright (`LOOM_CAPTURE_VIRTUAL_TIME=0`) → nothing to resume.
///
/// Pure so the resume policy is unit-testable without a live renderer; the three
/// `page_navigate` exit guards (success + the two CDP-error bail paths) all route
/// through it so the policy can never drift between them.
pub(crate) fn should_resume_virtual_clock(
    vt_active: bool,
    clock_pinned: bool,
    budget_drained: bool,
) -> bool {
    vt_active && !(clock_pinned && budget_drained)
}

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
        /// HTTP status of main document. Derived from the main-document
        /// event (`main_document_event_index`); falls back to 0 when no
        /// main-document event was captured.
        status_code: u16,
        /// Index into `network_events` of the event attributed to THIS
        /// navigation's main document (matched against the
        /// `Page.navigate` response's loaderId/frameId; see
        /// `find_main_document_index`). `None` = no event attributable
        /// to the main document (no events, or only iframe documents).
        /// The host scopes the navigate failure verdict (4xx/transport)
        /// to this event ONLY — iframe document errors stay in
        /// `network_events` for observability without failing the
        /// navigate. Control-flow plumbing only: never enters the
        /// hashed receipt. `serde(default)` for CBOR wire back-compat.
        #[serde(default)]
        main_document_event_index: Option<u32>,
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
    /// video-capture: per-target screencast recordings. Shares `cdp` so the
    /// recorder issues `Page.startScreencast`/`Ack`/`stopScreencast` on the
    /// same connection.
    pub(crate) recorder: Arc<crate::screencast_recorder::ScreencastRecorder>,
}

impl ChromiumActionExecutor {
    pub fn new(
        cdp: Arc<dyn CdpConnection>,
        network: Arc<dyn NetworkInterceptor>,
        targets: Arc<dyn TargetManager>,
    ) -> Self {
        let recorder = Arc::new(crate::screencast_recorder::ScreencastRecorder::new(
            cdp.clone(),
            Arc::new(crate::screencast_recorder::FfmpegSidecarEncoder),
        ));
        Self {
            cdp,
            network,
            targets,
            default_budget: DEFAULT_ACTION_BUDGET,
            recorder,
        }
    }

    /// Register a one-shot listener for a CDP event and return the receiver
    /// PLUS its RAII registration token. The handler fires the channel the
    /// first time `method` is observed. Register BEFORE issuing the command
    /// that triggers the event (fast events — cached/data: URL loads, an
    /// instantly-drained virtual-time budget — can otherwise fire before a
    /// post-command subscription lands). The caller must keep the token alive
    /// while awaiting the receiver: dropping it DEREGISTERS the handler, so
    /// per-navigate subscriptions no longer accumulate on the connection for
    /// the session's lifetime.
    fn subscribe_cdp_event_once(
        &self,
        method: &'static str,
    ) -> (oneshot::Receiver<()>, EventRegistration) {
        let signal: Arc<parking_lot::Mutex<Option<oneshot::Sender<()>>>> =
            Arc::new(parking_lot::Mutex::new(None));
        let (tx, rx) = oneshot::channel::<()>();
        *signal.lock() = Some(tx);
        let signal_for_handler = signal.clone();
        let handler: EventHandler = Arc::new(move |_target_id, msg: CdpMessage| {
            if msg.method == method {
                if let Some(tx) = signal_for_handler.lock().take() {
                    let _ = tx.send(());
                }
            }
        });
        let reg = self
            .cdp
            .register_event_handler(EventFilter::new(method), handler);
        (rx, reg)
    }

    /// animation-capture Mode A: best-effort RESUME of virtual time after a
    /// navigate. Issues `setVirtualTimePolicy{advance}` so a renderer left paused
    /// at the (possibly half-drained) budget horizon commits frames again and the
    /// NEXT standalone command (e.g. `Page.captureScreenshot`) is not wedged for
    /// the full 30s CDP timeout. Errors are swallowed — this is a cleanup guard,
    /// not a failure path — and it uses the caller's timeout so it can never wedge.
    async fn resume_virtual_time(&self, target_id: TargetId, timeout: Duration) {
        let msg = CdpMessage {
            method: crate::determinism_injector::determinism_injector::VIRTUAL_TIME_METHOD
                .to_string(),
            params:
                crate::determinism_injector::determinism_injector::build_virtual_time_resume_params(
                ),
        };
        // Bound this best-effort cleanup TIGHTLY (a single `setVirtualTimePolicy`
        // round-trip) so it can't add the full navigate budget's worth of latency to
        // an already-failing navigate, and so the cleanup itself can never wedge.
        let resume_to = timeout.min(RESUME_CDP_TIMEOUT);
        if let Err(e) = self.cdp.command(target_id, msg, Some(resume_to)).await {
            tracing::debug!(
                target_id,
                error = %e,
                "navigate: virtual-time resume (advance) failed; renderer may stay paused"
            );
        }
    }

    /// Re-arm the virtual-time budget for a NEW top-level document the page
    /// navigated to itself (client-nav-reattach). Under the determinism clock
    /// pin the new document's load is DEFERRED until a budget is armed (the same
    /// mechanism as the first navigation's STEP 4c). Subscribe to the new
    /// document's `Page.loadEventFired` + `virtualTimeBudgetExpired` BEFORE
    /// arming (so a stale event can't satisfy the wait), arm, then await both —
    /// each bounded by `timeout` (the caller passes the REMAINING action
    /// deadline, so the re-attach loop can never extend wall-clock past the
    /// action budget). Returns `(load_fired, budget_drained)`. Best-effort: a
    /// re-arm command failure logs and returns `(false, false)` so the caller
    /// falls through to a bounded settle → typed `timeout` rather than spinning.
    async fn rearm_for_reattach(&self, target_id: TargetId, timeout: Duration) -> (bool, bool) {
        // The three sequential awaits below (arm command, load wait, budget-drain
        // wait) SHARE a single deadline so one hop can never consume up to 3×
        // `timeout`. `timeout` is the caller's REMAINING action budget, so this
        // keeps the whole re-attach bounded by the action deadline (D10).
        let deadline = tokio::time::Instant::now() + timeout;
        let remaining = || deadline.saturating_duration_since(tokio::time::Instant::now());

        let (load_rx, _load_reg) = self.subscribe_cdp_event_once("Page.loadEventFired");
        let (vt_expired_rx, _vt_reg) = self.subscribe_cdp_event_once(
            crate::determinism_injector::determinism_injector::VIRTUAL_TIME_BUDGET_EXPIRED_EVENT,
        );
        let vt_budget = CdpMessage {
            method: crate::determinism_injector::determinism_injector::VIRTUAL_TIME_METHOD
                .to_string(),
            params:
                crate::determinism_injector::determinism_injector::build_virtual_time_budget_params(
                ),
        };
        if let Err(e) = self
            .cdp
            .command(target_id, vt_budget, Some(remaining()))
            .await
        {
            tracing::warn!(target_id, error = %e, "reattach: virtual-time budget re-arm failed");
            return (false, false);
        }
        let load_fired = tokio::time::timeout(remaining(), load_rx).await.is_ok();
        // Drain the budget so the re-settle + DOM capture on the new document are
        // virtual-time settled, exactly like STEP 4c does for the first nav.
        let budget_drained = tokio::time::timeout(remaining(), vt_expired_rx)
            .await
            .is_ok();
        (load_fired, budget_drained)
    }

    /// Drive the bounded STEP-4d / D9 client-nav re-attach chain shared by
    /// `page_navigate` and `wait_for`: while the page keeps self-initiating
    /// top-level navigations (`settle.renavigated`), re-arm the virtual-time
    /// budget (under the determinism clock pin) and re-settle on each new
    /// document, hop-capped by [`MAX_REATTACH_HOPS`] and all bounded by the SAME
    /// remaining action deadline (`reattach_start` + `timeout`). Determinism-safe
    /// (NFR-DET-01): `renavigated`/`hops` are shim-internal and never enter the
    /// manifest hash; this only moves code, changing no CDP ordering. Returns the
    /// final settle verdict plus the last re-arm hop's `budget_drained` (see
    /// [`ReattachOutcome`]).
    async fn settle_with_reattach(
        &self,
        target_id: TargetId,
        settle_mode: crate::readiness_monitor::SettleMode,
        timeout: Duration,
        reattach_start: tokio::time::Instant,
        mut settle: crate::readiness_monitor::SettleResult,
        kind: ReattachKind,
    ) -> ReattachOutcome {
        // Only re-arm the virtual-time budget on re-attach when THIS session
        // actually pinned the clock at inject. A `--no-determinism` session runs
        // on the real wall-clock and never pinned it, so re-arming is pointless
        // and harmful: the expiry event never reliably fires on a real-clock
        // page, hanging the re-settle until the deadline.
        let clock_pinned = crate::determinism_injector::determinism_injector::clock_pinned();
        let mut hops: u32 = 0;
        let mut last_drained: Option<bool> = None;
        while settle.renavigated && hops < MAX_REATTACH_HOPS {
            let remaining = timeout.saturating_sub(reattach_start.elapsed());
            if remaining.is_zero() {
                break;
            }
            hops += 1;
            tracing::debug!(
                target_id,
                hops,
                "{}: re-attaching to client-initiated top-level navigation",
                kind.verb()
            );
            let new_load = if clock_pinned {
                let (lf, drained) = self.rearm_for_reattach(target_id, remaining).await;
                last_drained = Some(drained);
                lf
            } else {
                // No determinism clock pin (or `--no-determinism`): the new
                // document loads on its own wall-clock; no budget to re-arm.
                true
            };
            let remaining = timeout.saturating_sub(reattach_start.elapsed());
            if remaining.is_zero() {
                break;
            }
            settle = crate::readiness_monitor::wait_for_settle(
                &self.cdp,
                target_id,
                settle_mode,
                crate::readiness_monitor::settle_driver::config_for_timeout(remaining),
                new_load,
                // the re-attached document was just (re-)loaded above.
                false,
                remaining,
            )
            .await;
        }
        if settle.renavigated {
            // Hit the hop cap or ran out of deadline mid-chain — capture/resolve
            // lands on the current (possibly in-flight) document and the receipt
            // records the bounded `timeout` outcome (D4). Surfaced so a
            // redirect-loop page is distinguishable from a slow one in logs.
            tracing::warn!(
                target_id,
                hops,
                max_hops = MAX_REATTACH_HOPS,
                "{}: re-attach chain exhausted hop cap / deadline; {}",
                kind.verb(),
                kind.exhausted_tail()
            );
        }
        ReattachOutcome {
            settle,
            last_drained,
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
        // cross-run determinism: when true (determinism on), await the
        // virtual-time budget to drain before DOM capture so timer-driven DOM
        // mutations have deterministically fired. `--no-determinism` → false.
        determinism_enabled: bool,
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

    /// video-capture: start a `Page.startScreencast` recording on `target_id`.
    /// `Err(detail)` (no session abort) when a recording is already active, the
    /// kill-switch is set, or `Page.startScreencast` fails. Default impl (non-
    /// Chromium executors) reports unsupported.
    async fn start_recording(
        &self,
        _target_id: TargetId,
        _caps: crate::screencast_recorder::Caps,
    ) -> Result<(), String> {
        Err("screencast recording not supported by this executor".to_string())
    }

    /// video-capture: stop the active recording, encode it, and return the
    /// outcome (errors are embedded in the outcome, never at the call boundary).
    async fn stop_recording(
        &self,
        _target_id: TargetId,
    ) -> loom_shared::navigate_outcome::ScreencastOutcome {
        loom_shared::navigate_outcome::ScreencastOutcome {
            stop_reason: "error".to_string(),
            error: Some("screencast recording not supported by this executor".to_string()),
            ..Default::default()
        }
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
        determinism_enabled: bool,
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
        // passing an explicit `budget` keep their value; otherwise the
        // env-configurable `navigate_budget()` applies (the dispatcher passes
        // `None`, so it is the binding per-CDP-command timeout in production).
        let timeout = budget.unwrap_or_else(navigate_budget);

        // STEP 1: subscribe to Page.loadEventFired BEFORE issuing navigate
        // (per practitioner bug magnet #1: cached / data: URLs fire fast,
        // and post-navigate subscription will miss the event). The RAII token
        // keeps the handler registered for this navigate only.
        let (load_rx, _load_reg) = self.subscribe_cdp_event_once("Page.loadEventFired");
        // (The virtual-time budget-expiry subscription happens in STEP 4c,
        // immediately before the budget is armed — NOT here. Subscribing at
        // navigate start left a multi-second window in which a STALE
        // `virtualTimeBudgetExpired` from a previous navigate's never-drained
        // budget could satisfy this navigate's wait before its budget was even
        // armed, letting DOM capture race the fresh budget.)

        // Reset the full-capture network-entries accumulator at navigate START
        // so this navigate's `network_entries` reflect only this navigate;
        // entries then accumulate across in-session clicks/evaluate until the
        // next navigate. CDP events arrive under target_id==0 (see the drain
        // note below); clear both keys to cover the fake-chromium per-target path.
        self.network.clear_entries(0);
        self.network.clear_entries(target_id);

        // Also drop any HASHED Document events left over from before this
        // navigate: a failed/aborted prior navigate early-returns past the
        // STEP 7 drain, and an in-session click can trigger a real link
        // navigation whose Document events land between navigates. Without
        // this, stale events poison the next receipt's `network_events` /
        // `status_code` (and the host's 4xx/transport failure verdict).
        // Clearing at START opens the hashed event window at Page.navigate
        // issue time, making capture per-navigation deterministic
        // (NFR-DET-01); the success path drained everything at STEP 7
        // already, so serial successful navigates record byte-identical
        // receipts either way.
        self.network.clear_events(0);
        self.network.clear_events(target_id);

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

        // STEP 4 + 4c: await Page.loadEventFired and (under virtual time) arm
        // + drain this navigation's bounded virtual-time budget
        // (faithful-entrance-animations + cross-run determinism). The budget is
        // a hard ceiling (security boundary) so a never-terminating animation
        // pauses instead of spinning the renderer unbounded.
        //
        // ORDER IS LOAD-BEARING and depends on whether the inject-time clock
        // pin is in effect:
        //
        // - determinism ON: inject sent `setVirtualTimePolicy {policy:"pause"}`,
        //   and headless Chromium DEFERS the load-completion tasks while
        //   virtual time is paused — `Network.responseReceived` arrives but
        //   `Page.loadEventFired` is held until the clock advances (verified
        //   live: the load event fires the instant the budget is armed).
        //   Waiting for load BEFORE arming therefore always burned the full
        //   wall-clock timeout, and the settle machine — whose networkidle/
        //   settled latches require `load_fired` — then reported `timeout` on
        //   perfectly healthy static pages (settle-timeout-on-static). So under
        //   determinism: arm the budget FIRST (it is what lets the load
        //   complete), then await load, then await the budget expiry so DOM
        //   capture stays virtual-time settled (commit 114be83).
        // - determinism OFF: the clock was never pinned (inject skipped), so
        //   keep the original order — await load, then arm post-load (arming
        //   before the load is in flight can make the expiry event unreliable
        //   under pauseIfNetworkFetchesPending, chrome-headless-render-pdf#29).
        let vt_active = crate::determinism_injector::determinism_injector::virtual_time_enabled();
        let clock_pinned = vt_active && determinism_enabled;

        let mut load_rx = Some(load_rx);
        let mut load_fired = if clock_pinned {
            // Load cannot complete while the inject-time pin holds; resolved
            // after the budget is armed below.
            false
        } else {
            // The fake-chromium harness emits the event right after the
            // Page.navigate response so this typically completes immediately.
            // Real Chromium can take longer.
            let rx = load_rx.take().expect("load_rx consumed once");
            tokio::time::timeout(timeout, rx).await.is_ok()
        };

        // animation-capture (Mode A): tracks whether THIS navigate's virtual-time
        // budget was confirmed drained. A cleanly drained budget leaves the
        // renderer at a screenshottable horizon with a frozen (deterministic)
        // clock — no resume needed, so determinism is preserved on the clean path.
        // A NOT-drained budget (rearm fail / drain timeout / capture error) can
        // leave the renderer paused mid-flight, wedging the next command for the
        // full 30s CDP timeout — so we resume (`advance`) on exit ONLY then.
        let mut budget_drained = false;

        // Only drive virtual time when THIS session actually pinned the clock at
        // inject (`clock_pinned`). A `--no-determinism` session has `vt_active`
        // true (the global capture flag) but runs on the REAL wall-clock — arming
        // a virtual-time budget here makes `virtualTimeBudgetExpired` unreliable
        // (it never fires while a cross-origin iframe keeps network fetches
        // pending under `pauseIfNetworkFetchesPending`), so navigate burned the
        // full timeout and, on a heavy auth'd SPA, wedged the next navigate. Gate
        // on `clock_pinned`, not `vt_active`, so no_determinism takes the clean
        // real-clock load+settle path. (No replay impact: no_determinism sessions
        // are non-replayable.)
        if clock_pinned {
            // Subscribe to the budget-expiry event immediately BEFORE issuing
            // the arm command — after the prior navigate's awaits, not at
            // navigate start. The WS command/response round-trip orders this
            // subscription ahead of THIS navigate's expiry event, while
            // shrinking the window in which a stale expiry from a previous
            // navigate's never-drained budget (the warn path below) could
            // satisfy the wait prematurely from seconds to sub-millisecond.
            // (A stale `Page.loadEventFired` has the same residual hazard;
            // with the pin-aware ordering above the load wait can no longer
            // time out on healthy pages, which is what left budgets undrained.)
            let (vt_expired_rx, _vt_reg) = self.subscribe_cdp_event_once(
                crate::determinism_injector::determinism_injector::VIRTUAL_TIME_BUDGET_EXPIRED_EVENT,
            );
            let vt_budget = CdpMessage {
                method: crate::determinism_injector::determinism_injector::VIRTUAL_TIME_METHOD
                    .to_string(),
                params:
                    crate::determinism_injector::determinism_injector::build_virtual_time_budget_params(
                    ),
            };
            if let Err(e) = self.cdp.command(target_id, vt_budget, Some(timeout)).await {
                // Best-effort: the page stays on the inject clock. Under the
                // pin that also means the load CANNOT complete — skip the load
                // wait (it would burn the full timeout) and fall through to
                // the bounded settle, which returns a typed `timeout`.
                tracing::warn!(target_id, error = %e, "navigate: virtual-time budget re-arm failed");
            } else {
                // `load_rx` is still Some exactly when the clock was pinned
                // (the not-pinned branch consumed it pre-arm).
                if let Some(rx) = load_rx.take() {
                    load_fired = tokio::time::timeout(timeout, rx).await.is_ok();
                }
                // animation-capture (Mode C, C3): block DOM capture until the
                // virtual-time budget drains REGARDLESS of `determinism_enabled`.
                // Previously this awaited only under determinism, so a
                // `--no-determinism` (or budget-rearm) capture raced the reveal and
                // grabbed a pre-reveal `opacity:0` frame. All ≤budget virtual timers
                // (e.g. a setTimeout/whileInView reveal) fire first, making the
                // captured DOM stable. Bounded-determinism: a pathological page can
                // exhaust the wall-clock timeout before the budget elapses; on
                // timeout we warn + fall through to the existing settle (non-fatal)
                // so capture never hangs (D-INTAKE), and `budget_drained` stays
                // false so the exit guard resumes the renderer.
                budget_drained = tokio::time::timeout(timeout, vt_expired_rx).await.is_ok();
                if budget_drained {
                    tracing::debug!(
                        target_id,
                        "navigate: virtualTimeBudgetExpired arrived; DOM capture is virtual-time settled"
                    );
                } else {
                    tracing::warn!(
                        target_id,
                        "navigate: virtualTimeBudgetExpired did not arrive within timeout; \
                         settle may be non-deterministic — falling back to wait_for_settle"
                    );
                }
            }
        }

        // STEP 4b (settle-capture): gate the capture on the requested readiness
        // state. The verdict is a pure function of the per-tick observation
        // sequence in virtual ticks (DET-CORE), so it is replay-equal. `load` mode
        // returns immediately once load fired; networkidle/settled poll until quiet
        // or the tick ceiling (a bounded, typed fallback). animation-
        // capture Mode C: intersection-gated `whileInView` reveals fire AT MOUNT
        // (the deterministic IntersectionObserver override installed at inject —
        // see `determinism_injector::REVEAL_IO_OVERRIDE_JS`), so by the time the
        // budget above has drained they have animated to completion and the settled
        // capture below is post-reveal, not a pre-reveal blank.
        let reattach_start = tokio::time::Instant::now();
        let mut settle = crate::readiness_monitor::wait_for_settle(
            &self.cdp,
            target_id,
            settle_mode,
            crate::readiness_monitor::settle_driver::config_for_timeout(timeout),
            load_fired,
            // navigate's first settle ticks may transiently read `loading`
            // before the freshly-loaded document reflects `complete`.
            false,
            timeout,
        )
        .await;

        // STEP 4d (client-nav-reattach): if the loaded page began a top-level
        // navigation it initiated itself (window.location / <meta refresh> /
        // form-POST), the new document is wedged `loading` under the paused
        // virtual clock. Re-arm its budget and re-settle on it, following a
        // bounded redirect chain under the SAME action deadline (D4). DOM capture
        // below then lands on the FINAL settled document, not the blank shell.
        let outcome = self
            .settle_with_reattach(
                target_id,
                settle_mode,
                timeout,
                reattach_start,
                settle,
                ReattachKind::Navigate,
            )
            .await;
        settle = outcome.settle;
        // Carry the final re-arm hop's drained state back to the renderer-resume
        // guards below (only changes when a vt-arm hop actually ran).
        if let Some(drained) = outcome.last_drained {
            budget_drained = drained;
        }

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
        let dom_result = match self.cdp.command(target_id, dom_msg, Some(timeout)).await {
            Ok(r) => r,
            Err(e) => {
                // animation-capture (Mode A): un-pause before bailing so the next
                // command isn't wedged on a paused clock (unless we are on the
                // determinism-pinned clean-drain path — see the success-path guard).
                if should_resume_virtual_clock(clock_pinned, clock_pinned, budget_drained) {
                    self.resume_virtual_time(target_id, timeout).await;
                }
                return Err(action_error_to_response(ActionError::Cdp(e), 0, None));
            }
        };
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
        let shot_result = match self.cdp.command(target_id, shot_msg, Some(timeout)).await {
            Ok(r) => r,
            Err(e) => {
                // animation-capture (Mode A): un-pause before bailing (see STEP 5
                // and the success-path guard for the determinism-pinned exception).
                if should_resume_virtual_clock(clock_pinned, clock_pinned, budget_drained) {
                    self.resume_virtual_time(target_id, timeout).await;
                }
                return Err(action_error_to_response(ActionError::Cdp(e), 0, None));
            }
        };
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
        let mut drained = self.network.drain_events_attributed(0);
        if drained.is_empty() {
            drained = self.network.drain_events_attributed(target_id);
        }
        let mut blocked_events = self.network.drain_blocked(0);
        if blocked_events.is_empty() {
            blocked_events = self.network.drain_blocked(target_id);
        }

        // Identify THIS navigation's main-document event by matching each
        // event's frame/loader attribution against the Page.navigate
        // response's frameId/loaderId. Iframe document events (different
        // frame/loader) stay in `network_events` for observability but
        // must not drive `status_code` or the host's failure verdict.
        let mut main_document_event_index =
            find_main_document_index(&drained, &frame_id, &loader_id);
        let mut network_events: Vec<LoomNetworkEvent> =
            drained.into_iter().map(|(event, _)| event).collect();

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

        // Surface DNS / network failures via a synthetic LoomNetworkEvent
        // carrying `error_reason`. CDP `Page.navigate` returns a non-empty
        // `errorText` field when navigation could not even reach a server
        // (DNS, conn-refused, TLS, etc.). The host-side `navigate_execute`
        // detects a main-document event with `error_reason.is_some()` and
        // converts it into an HostError::ShimFailure carrying a structured
        // JSON detail (kind="dns_failure").
        if let Some(err_text) = extract_nav_error_text(&nav_response) {
            // Classify at the boundary (D-01): the shim owns chromium-
            // specific error mapping, the host reads the typed kind.
            let kind = classify_chromium_nav_error(&err_text).to_string();
            network_events.push(LoomNetworkEvent {
                // A page navigation is always a GET; populate it so failed-run HAR
                // evidence carries the method (deterministic → safe in the hash).
                // No HTTP response → response_bytes stays 0.
                method: "GET".to_string(),
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
            // The navigation itself failed, so the synthetic event IS the
            // main-document verdict — UNLESS a captured main-document
            // response already carries the HTTP status (chromium emits
            // ERR_HTTP_RESPONSE_CODE_FAILURE alongside a 4xx/5xx
            // responseReceived for empty-body responses; the actual
            // status_code is the more actionable verdict — see the
            // HTTP-first ordering note in `navigate_execute`).
            let has_http_verdict = main_document_event_index
                .and_then(|i| network_events.get(i as usize))
                .is_some_and(|e| e.status >= 400);
            if !has_http_verdict {
                main_document_event_index = Some((network_events.len() - 1) as u32);
            }
        }

        // Derive status_code from the MAIN document's event; fall back to 0
        // when no event is attributable to the main document. The synthetic
        // error event above has status=0, so a transport failure reports
        // status_code=0 exactly as before.
        let status_code = main_document_event_index
            .and_then(|i| network_events.get(i as usize))
            .map(|e| e.status)
            .unwrap_or(0);

        // animation-capture (Mode A): on the SUCCESS path resume the renderer
        // UNLESS we are on the determinism-pinned clean-drain path. A cleanly
        // drained budget UNDER the determinism clock pin leaves a screenshottable
        // horizon with a frozen, deterministic clock, so we DON'T resume it
        // (preserving replay-equality for a subsequent web.evaluate clock read).
        // With determinism OFF there is no replay contract, so a frozen clock is
        // pure harm: it defers the NEXT navigate's Page.loadEventFired and wedges
        // the determinism-OFF navigate path into burning its full budget every
        // call (navigate-degradation). `should_resume_virtual_clock` encodes the
        // policy once for all three exit guards.
        if should_resume_virtual_clock(clock_pinned, clock_pinned, budget_drained) {
            self.resume_virtual_time(target_id, timeout).await;
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
            main_document_event_index,
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
        let timeout = budget.unwrap_or_else(navigate_budget);
        // Gate the virtual-time arm+await on whether this session actually pinned
        // the clock at inject. A `--no-determinism` session never pinned it, so
        // arming a budget here would await a `virtualTimeBudgetExpired` that never
        // reliably fires on the real wall-clock (e.g. while a cross-origin
        // silent-auth iframe keeps network fetches pending) — the wedge behind the
        // 2nd authed action on a heavy SPA. (No replay impact: non-replayable.)
        let clock_pinned = crate::determinism_injector::determinism_injector::clock_pinned();

        // settle-drives-pending-timers (auth0-ulp-submit): under the determinism
        // clock pin the virtual clock is FROZEN here — the prior navigate/settle
        // drained its budget and a cleanly-drained budget is deliberately left
        // paused (see page_navigate STEP 4c / the exit-resume note: only an
        // UNDRAINED budget resumes to `advance`). A preceding interaction verb
        // (web.click / web.press_key — host-side raw `Input.*` passthroughs that
        // arm no budget of their own) can run a page handler that schedules async
        // work behind a timer: e.g. Auth0 New Universal Login's react-hook-form
        // onSubmit does async validate → `navigator.credentials` WebAuthn probe →
        // `fetch(POST /u/login/identifier)`. With the clock frozen that chain
        // stalls at its first macrotask/`setTimeout` await — BEFORE it ever issues
        // the request — so the page never begins the navigation this wait_for is
        // meant to observe, and the old "no-vt-arm common path" settled the still-
        // `complete` document immediately (the bug).
        //
        // Arm a bounded virtual-time budget HERE — the same helper + ordering
        // navigate STEP 4c uses (subscribe to the expiry BEFORE arming so a stale
        // event can't satisfy the wait) — so pending timers fire and any resulting
        // top-level navigation begins. The settle + reattach below then observe and
        // resolve it on the new document. Determinism-safe (NFR-DET-01): the VT
        // control commands are shim-internal and excluded from the manifest hash
        // (navigate arms budgets the same way); the settle verdict stays a pure
        // function of the deterministic per-tick observation sequence, so it is
        // replay-equal. A page with no pending timers drains the budget immediately
        // (one extra CDP round-trip), unchanged verdict.
        let mut initial_budget_drained = true;
        if clock_pinned {
            let (vt_expired_rx, _vt_reg) = self.subscribe_cdp_event_once(
                crate::determinism_injector::determinism_injector::VIRTUAL_TIME_BUDGET_EXPIRED_EVENT,
            );
            let vt_budget = CdpMessage {
                method: crate::determinism_injector::determinism_injector::VIRTUAL_TIME_METHOD
                    .to_string(),
                params:
                    crate::determinism_injector::determinism_injector::build_virtual_time_budget_params(
                    ),
            };
            if let Err(e) = self.cdp.command(target_id, vt_budget, Some(timeout)).await {
                tracing::warn!(target_id, error = %e, "wait_for: virtual-time budget arm failed");
                initial_budget_drained = false;
            } else {
                initial_budget_drained = tokio::time::timeout(timeout, vt_expired_rx).await.is_ok();
            }
        }

        let reattach_start = tokio::time::Instant::now();
        let mut settle = crate::readiness_monitor::wait_for_settle(
            &self.cdp,
            target_id,
            settle_mode,
            crate::readiness_monitor::settle_driver::config_for_timeout(timeout),
            true,
            // the caller asserts the page already loaded, so a `loading`
            // observation here means the page renavigated (e.g. a form-POST
            // submitted just before this wait_for) — detect + re-attach.
            true,
            timeout,
        )
        .await;

        // client-nav-reattach (D9): the initial budget arm above advanced pending
        // timers; if that began a top-level navigation the settle reads `loading`
        // and the vt-arm reattach path re-settles on the new document, giving
        // wait_for the same bounded re-attach as navigate instead of hanging.
        let outcome = self
            .settle_with_reattach(
                target_id,
                settle_mode,
                timeout,
                reattach_start,
                settle,
                ReattachKind::WaitFor,
            )
            .await;
        settle = outcome.settle;
        // A budget left undrained — either the initial arm above (arm failure /
        // drain timeout) or the final reattach hop — would wedge the next command
        // on a paused clock mid-flight, so resume to `advance` on exit. A cleanly
        // drained budget (the common case) is deliberately left paused/frozen,
        // matching navigate's clean-path exit so a subsequent web.evaluate clock
        // read stays replay-equal.
        let needs_resume = !initial_budget_drained || matches!(outcome.last_drained, Some(false));
        if clock_pinned && needs_resume {
            self.resume_virtual_time(target_id, timeout).await;
        }

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

    async fn start_recording(
        &self,
        target_id: TargetId,
        caps: crate::screencast_recorder::Caps,
    ) -> Result<(), String> {
        self.recorder.start(target_id, caps).await
    }

    async fn stop_recording(
        &self,
        target_id: TargetId,
    ) -> loom_shared::navigate_outcome::ScreencastOutcome {
        self.recorder.stop(target_id).await
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

/// Find the index of the event attributed to THIS navigation's main
/// document. An event "matches" the navigation when its loaderId equals
/// the `Page.navigate` response's loaderId (preferred — each navigation
/// gets a fresh loader, so late events from a superseded prior load are
/// excluded), else when its frameId equals the navigated (main) frame,
/// else — when neither side carries identifiers — unconditionally
/// (conservative fallback that preserves the pre-attribution failure
/// semantics for harnesses/events without frame plumbing).
///
/// Among matching events, prefer the first carrying an HTTP status
/// (responseReceived) over a transport failure (loadingFailed): chromium
/// pairs ERR_HTTP_RESPONSE_CODE_FAILURE with a 4xx/5xx responseReceived
/// for empty-body responses, and the status is the more actionable
/// verdict (HTTP-first ordering, mirrored host-side in
/// `navigate_execute`).
pub(super) fn find_main_document_index(
    events: &[(LoomNetworkEvent, EventAttribution)],
    nav_frame_id: &str,
    nav_loader_id: &str,
) -> Option<u32> {
    let matching = |attribution: &EventAttribution| {
        attribution_matches_navigation(attribution, nav_frame_id, nav_loader_id)
    };
    let with_status = events
        .iter()
        .position(|(event, attribution)| matching(attribution) && event.status > 0);
    let with_error = events
        .iter()
        .position(|(event, attribution)| matching(attribution) && event.error_reason.is_some());
    let any = events
        .iter()
        .position(|(_, attribution)| matching(attribution));
    with_status.or(with_error).or(any).map(|i| i as u32)
}

/// Whether an event's frame/loader attribution ties it to the navigation
/// identified by `nav_frame_id`/`nav_loader_id`. See
/// `find_main_document_index` for the matching policy.
fn attribution_matches_navigation(
    attribution: &EventAttribution,
    nav_frame_id: &str,
    nav_loader_id: &str,
) -> bool {
    if !attribution.loader_id.is_empty() && !nav_loader_id.is_empty() {
        return attribution.loader_id == nav_loader_id;
    }
    if !attribution.frame_id.is_empty() && !nav_frame_id.is_empty() {
        return attribution.frame_id == nav_frame_id;
    }
    // Unattributed event or navigation without identifiers: treat as
    // main-document so transport failures are never silently dropped.
    true
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
