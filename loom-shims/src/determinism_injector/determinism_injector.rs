// DeterminismInjector — R3 mitigation owner.
//
// # Contract semantics
// - **R3 LOAD-BEARING (KILL).** Every CDP target attach
//   triggers `Page.addScriptToEvaluateOnNewDocument({source:
//   <determinism_init.js>, runImmediately: true})` BEFORE any user
//   navigation begins. With `runImmediately: true` the script fires
//   on every new document INCLUDING the first navigation. Deferred
//   injection (post-`Page.loadEventFired`) → KILL.
// - **R3 invariant guard.** After successful injection, callers
//   (`TargetManager::create_new_target`) flip
//   `TargetState.determinism_injected = true`. Any navigation against
//   a target whose flag is still `false` is rejected by
//   `ActionExecutor::page_navigate`.
// - **Subscription is callback-based (acyclicity).**
//   `DeterminismInjector::new` registers a `Page.frameStartedLoading`
//   handler in `CdpConnection`; `CdpConnection` does NOT import this
//   module. The handler is a defensive no-op for already-injected
//   targets — primary injection happens via the explicit
//   `inject(target_id)` call from `TargetManager`.
// - **Determinism source script.** `determinism_init.js` installs:
//   1. Seeded RNG (`Math.random` derived from session seed).
//   2. CSS animation/transition flattening (resolve to end-state).
//   The clocks (`Date.now`/`performance.now`/`requestAnimationFrame`/
//   `setTimeout`) are NO LONGER overridden in JS — they are driven by CDP
//   `Emulation.setVirtualTimePolicy` (a deterministic, epoch-pinned, advancing
//   virtual timeline) so entrance animations render while staying replay-equal
//   (faithful-entrance-animations). `inject` pins the origin (`pause` policy);
//   each navigation re-arms a bounded budget. The script source is shipped
//   beside the binary at bundle install time; `Supervisor::start` resolves the
//   bundle path.
// - **Idempotent.** Calling `inject(target_id)` twice on the same
//   target is a no-op for the second call (chromiumoxide's
//   `addScriptToEvaluateOnNewDocument` returns a different identifier
//   each time, but the script source is identical).

use crate::cdp_connection::cdp_connection::{CdpConnection, CdpError, EventRegistration};
use crate::determinism_script_template::render_determinism_script;
use crate::ipc_endpoint::ipc_endpoint::{CdpMessage, ShimErrorCode, TargetId};
use loom_shared::types::{EpochMs, Seed};
use std::sync::Arc;

/// CDP method name used to install the script. Public so tests can
/// assert string equality.
pub const ADD_SCRIPT_METHOD: &str = "Page.addScriptToEvaluateOnNewDocument";

/// `runImmediately: true` is load-bearing for R3 — without it the script
/// fires on the SECOND navigation only.
pub const RUN_IMMEDIATELY: bool = true;

/// CDP method that installs the deterministic, advancing virtual-time clock
/// (faithful-entrance-animations). Driving `Date.now`/`performance.now`/`rAF`/
/// `setTimeout` off one coherent virtual timeline lets client-side entrance
/// animations run to completion while staying replay-equal.
pub const VIRTUAL_TIME_METHOD: &str = "Emulation.setVirtualTimePolicy";

/// CDP event fired once the per-navigation virtual-time budget elapses. Awaited
/// before DOM capture under determinism so all ≤budget virtual timers (e.g. a
/// `setTimeout`-driven reveal) have deterministically fired — making the captured
/// `dom_snapshot_hash` cross-run-stable. See `action_executor::page_navigate`.
pub const VIRTUAL_TIME_BUDGET_EXPIRED_EVENT: &str = "Emulation.virtualTimeBudgetExpired";

/// Deadlock guard for `build_virtual_time_budget_params`. A pathological page
/// (busy `setInterval` loop) could otherwise pin virtual time so the budget never
/// elapses; CDP forces virtual time forward after this many tasks. High enough not
/// to perturb normal pages, finite so `virtualTimeBudgetExpired` always arrives.
pub const VIRTUAL_TIME_MAX_TASK_STARVATION: i64 = 1_000_000;

/// Virtual-time policy. `pauseIfNetworkFetchesPending` advances time through
/// timers/rAF/animations but pauses while network fetches are in flight, so the
/// page still loads its resources before the clock fast-forwards.
pub const VIRTUAL_TIME_POLICY: &str = "pauseIfNetworkFetchesPending";

/// Per-navigation virtual-time budget (ms). Bounds how far the clock
/// fast-forwards for a single navigation: enough for entrance animations to
/// complete, but a hard ceiling so a never-terminating animation (or a page
/// abusing `setTimeout` time-travel) can't spin virtual time unbounded. Also
/// the documented security boundary (council "unconstrained budget / time-travel").
pub const VIRTUAL_TIME_BUDGET_MS: f64 = 8000.0;

/// Env flag (P6 rollback): set `LOOM_CAPTURE_VIRTUAL_TIME=0` to disable the
/// virtual-time clock and fall back to the prior (frozen-clock-equivalent)
/// behavior. Default on.
pub fn virtual_time_enabled() -> bool {
    std::env::var("LOOM_CAPTURE_VIRTUAL_TIME").as_deref() != Ok("0")
}

/// Process-global: did THIS session pin the virtual-time clock at inject?
/// The shim is one-session-per-process, and the determinism freeze-inject runs
/// once per session and ONLY when `determinism_enabled` (see
/// `target_manager::create_new_target`). A `--no-determinism` session never
/// injects, so this stays `false` — letting navigate/`wait_for` skip the
/// virtual-time arm+await (whose `virtualTimeBudgetExpired` never reliably
/// arrives on a real wall-clock page, which is what hung the 2nd navigate to a
/// heavy cross-origin auth'd SPA). Set once at inject; read on the verb path.
static CLOCK_PINNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the virtual-time clock is pinned for this session (set at inject).
/// Verbs gate their virtual-time arm+await on this so `--no-determinism`
/// sessions take the clean real-clock load+settle path.
pub fn clock_pinned() -> bool {
    CLOCK_PINNED.load(std::sync::atomic::Ordering::SeqCst)
}

/// Record at inject whether this session pinned the virtual-time clock. Called
/// from the freeze-inject path (which only runs when `determinism_enabled`).
pub(crate) fn set_clock_pinned(pinned: bool) {
    CLOCK_PINNED.store(pinned, std::sync::atomic::Ordering::SeqCst);
}

/// Pure helper: build the CBOR params for re-arming the virtual-time budget on a
/// navigation (no `initialVirtualTime` — the origin was pinned at inject; this
/// just grants a bounded budget for the page to advance through during load +
/// animation).
pub fn build_virtual_time_budget_params() -> ciborium::value::Value {
    use ciborium::value::Value;
    Value::Map(vec![
        (
            Value::Text("policy".into()),
            Value::Text(VIRTUAL_TIME_POLICY.into()),
        ),
        (
            Value::Text("budget".into()),
            Value::Float(VIRTUAL_TIME_BUDGET_MS),
        ),
        // Forces virtual time forward after N tasks so a busy-loop page can't pin
        // it and starve `virtualTimeBudgetExpired` (which we await before capture).
        (
            Value::Text("maxVirtualTimeTaskStarvationCount".into()),
            Value::Integer(VIRTUAL_TIME_MAX_TASK_STARVATION.into()),
        ),
    ])
}

/// Virtual-time RESUME policy. `advance` lets virtual time progress freely
/// (never pausing), so a renderer left paused at a drained/half-drained budget
/// horizon will commit frames again. Used by `page_navigate`'s exit guard
/// (animation-capture Mode A) to un-pause the renderer on EVERY navigate exit so
/// the next standalone command (e.g. `Page.captureScreenshot`) is not wedged for
/// the full 30s CDP timeout ("Daemon unresponsive after 30s").
pub const VIRTUAL_TIME_RESUME_POLICY: &str = "advance";

/// Pure helper: build the CBOR params for the navigate-exit virtual-time RESUME.
/// `{policy:"advance"}` with NO `budget` — a budget would re-pause at expiry and
/// re-introduce the wedge this is meant to clear (animation-capture Mode A; the
/// `virtual_time_resume_params_advance_without_budget` contract test pins this).
pub fn build_virtual_time_resume_params() -> ciborium::value::Value {
    use ciborium::value::Value;
    Value::Map(vec![(
        Value::Text("policy".into()),
        Value::Text(VIRTUAL_TIME_RESUME_POLICY.into()),
    )])
}

/// animation-capture Mode C — deterministic `IntersectionObserver` override.
///
/// framer-motion `whileInView` (and every intersection-gated reveal) sits at
/// `opacity:0` until its element scrolls into view. loom's capture never scrolls,
/// and driving the reveal by SCROLLING under CDP virtual time proved unreliable —
/// `IntersectionObserver` delivery is a sub-frame race that fires the reveal only
/// intermittently in real Chrome. Instead, this script (injected before the page's
/// own scripts, via `addScriptToEvaluateOnNewDocument`) replaces
/// `IntersectionObserver` with a shim that reports every observed element as fully
/// intersecting on a microtask. So whileInView reveals fire AT MOUNT — exactly like
/// a time/mount-triggered reveal, which virtual time already fast-forwards to
/// completion before capture. Fully deterministic (fixed script, microtask delivery
/// under virtual time) and replay-equal. Semantics: "capture the page as if every
/// observed element is in view", which is the right capture intent.
///
/// TRADE-OFF (intentional): this also fires NON-reveal IntersectionObserver uses —
/// lazy-load, infinite scroll, analytics-in-view, sticky-header toggles. For a
/// capture (we want the fully-materialized page) that is usually desirable, and it
/// is BOUNDED: any lazy-loaded work runs under the per-navigate virtual-time budget
/// and the settle tick ceiling, so an infinite-scroll page can't load unbounded. For
/// pages where it is wrong, the env kill-switch `LOOM_CAPTURE_REVEAL=0` skips
/// installing it (the page keeps the real IntersectionObserver). Errors are swallowed
/// (try/catch) so a malformed environment never breaks capture.
pub const REVEAL_IO_OVERRIDE_JS: &str = r#"(function(){'use strict';
  try {
    if (typeof window.IntersectionObserver !== 'function') return;
    if (window.__loomIoOverridden) return;
    window.__loomIoOverridden = true;
    function LoomIO(cb){ this._cb = cb; }
    LoomIO.prototype.observe = function(el){
      var cb = this._cb, self = this;
      Promise.resolve().then(function(){
        try {
          var r = (el && el.getBoundingClientRect) ? el.getBoundingClientRect() : null;
          cb([{
            target: el,
            isIntersecting: true,
            intersectionRatio: 1,
            boundingClientRect: r,
            intersectionRect: r,
            rootBounds: r,
            time: 0
          }], self);
        } catch(e) {}
      });
    };
    LoomIO.prototype.unobserve = function(){};
    LoomIO.prototype.disconnect = function(){};
    LoomIO.prototype.takeRecords = function(){ return []; };
    LoomIO.prototype.root = null;
    LoomIO.prototype.rootMargin = '0px';
    LoomIO.prototype.thresholds = [0];
    window.IntersectionObserver = LoomIO;
  } catch(e) {}
})()"#;

/// Whether to install the deterministic IntersectionObserver override
/// (animation-capture Mode C). Default on; `LOOM_CAPTURE_REVEAL=0` disables
/// it (shared kill-switch with the legacy scroll path).
pub fn reveal_capture_enabled() -> bool {
    std::env::var("LOOM_CAPTURE_REVEAL").as_deref() != Ok("0")
}

/// Deterministic clock-FREEZE fallback. Injected ONLY when virtual time is
/// disabled (`LOOM_CAPTURE_VIRTUAL_TIME=0`) or when arming it fails — so the
/// rollback / failure path restores the prior *deterministic* frozen-clock
/// behavior (replay-equal, animations don't run) rather than leaking a
/// non-deterministic real-time clock. `__LOOM_EPOCH_MS__` is substituted with
/// the session epoch. Kept separate from `determinism_init.js` (the main asset
/// stays freeze-free so the default virtual-time path is unaffected).
pub const CLOCK_FREEZE_JS: &str = r#"(function(){'use strict';
  var _e = __LOOM_EPOCH_MS__;
  Date.now = function(){ return _e; };
  Date.prototype.getTime = function(){ return _e; };
  if (typeof performance !== 'undefined') {
    try { Object.defineProperty(performance, 'timeOrigin', { get:function(){return _e;}, configurable:true }); } catch(e){}
    performance.now = function(){ return 0; };
  }
  var _t = 0;
  window.requestAnimationFrame = function(cb){ _t += 16; var t = _t; setTimeout(function(){ cb(t); }, 0); return _t; };
  window.cancelAnimationFrame = function(_id){};
})()"#;

/// Render the clock-freeze fallback with the session epoch.
pub fn render_clock_freeze(epoch_ms: u64) -> String {
    CLOCK_FREEZE_JS.replace("__LOOM_EPOCH_MS__", &epoch_ms.to_string())
}

/// Pure helper: build the CBOR params for the INJECT-time
/// `Emulation.setVirtualTimePolicy`. Uses the `pause` policy with
/// `initialVirtualTime` (seconds since epoch) to PIN the clock origin to the
/// session epoch without advancing — so the empty about:blank document does not
/// burn any budget before the first navigation. Each navigation then re-arms a
/// bounded budget (`build_virtual_time_budget_params`) to actually advance the
/// clock through load + animations. Pinning the origin makes `Date.now()` /
/// `new Date()` deterministic and coherent with `performance.now()`.
pub fn build_virtual_time_params(epoch_ms: u64) -> ciborium::value::Value {
    use ciborium::value::Value;
    let epoch_secs = epoch_ms as f64 / 1000.0;
    Value::Map(vec![
        (Value::Text("policy".into()), Value::Text("pause".into())),
        (
            Value::Text("initialVirtualTime".into()),
            Value::Float(epoch_secs),
        ),
    ])
}

/// Concrete DeterminismInjector.
pub struct ChromiumDeterminismInjector {
    pub(crate) cdp: Arc<dyn CdpConnection>,
    pub(crate) script_source: String,
    pub(crate) per_target_identifiers:
        parking_lot::RwLock<std::collections::BTreeMap<TargetId, String>>,
    pub(crate) registration: parking_lot::Mutex<Option<EventRegistration>>,
}

impl ChromiumDeterminismInjector {
    /// Construct + register the defensive `Page.*` handler with
    /// `CdpConnection`. The `script_source` is the contents of
    /// `determinism_init.js` (read at boot from the bundle path).
    pub fn new(cdp: Arc<dyn CdpConnection>, script_source: String) -> Arc<Self> {
        Arc::new(Self {
            cdp,
            script_source,
            per_target_identifiers: parking_lot::RwLock::new(Default::default()),
            registration: parking_lot::Mutex::new(None),
        })
    }
}

/// Public DeterminismInjector trait surface. `inject` is async because
/// the underlying `CdpConnection::command` is async (L4 wired the WS+JSON
/// transport). `clear_target` and `script_source_present` stay sync.
#[async_trait::async_trait]
pub trait DeterminismInjector: Send + Sync {
    /// Inject the determinism script for `target_id`, rendering the
    /// per-session `seed` and `epoch_ms` into the template at call time.
    /// Called by `TargetManager::create_new_target` BEFORE `Network.enable`
    /// / `Page.enable`. After this returns Ok, the caller flips
    /// `TargetState.determinism_injected = true`.
    async fn inject(
        &self,
        target_id: TargetId,
        seed: Seed,
        epoch_ms: EpochMs,
    ) -> Result<(), DeterminismError>;

    /// Remove the script registration for a target.
    fn clear_target(&self, target_id: TargetId);

    /// Whether the script source is non-empty.
    fn script_source_present(&self) -> bool;
}

#[derive(Debug, thiserror::Error)]
pub enum DeterminismError {
    #[error("CDP failure during injection: {0}")]
    CdpFailure(#[from] CdpError),
    #[error("target {0} detached during inject")]
    TargetDetached(TargetId),
    #[error("script source is empty")]
    EmptySource,
}

impl From<DeterminismError> for ShimErrorCode {
    fn from(e: DeterminismError) -> Self {
        match e {
            DeterminismError::CdpFailure(c) => c.into(),
            DeterminismError::TargetDetached(_) => ShimErrorCode::ChromiumUnavailable,
            DeterminismError::EmptySource => ShimErrorCode::ShimInternalError,
        }
    }
}

#[async_trait::async_trait]
impl DeterminismInjector for ChromiumDeterminismInjector {
    async fn inject(
        &self,
        target_id: TargetId,
        seed: Seed,
        epoch_ms: EpochMs,
    ) -> Result<(), DeterminismError> {
        if self.script_source.is_empty() {
            tracing::error!(target_id, "determinism: empty source — refusing to inject");
            return Err(DeterminismError::EmptySource);
        }
        if self.per_target_identifiers.read().contains_key(&target_id) {
            return Ok(());
        }
        tracing::info!(
            target_id,
            seed = seed.0,
            epoch_ms = epoch_ms.0,
            "determinism: inject start"
        );
        let rendered = render_determinism_script(&self.script_source, seed.0, epoch_ms.0);
        let params = build_inject_params(&rendered);
        let msg = CdpMessage {
            method: ADD_SCRIPT_METHOD.to_string(),
            params,
        };
        let response = match self.cdp.command(target_id, msg, None).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(
                    target_id,
                    seed = seed.0,
                    error = %e,
                    "determinism: inject failed"
                );
                return Err(e.into());
            }
        };
        let identifier = if let ciborium::value::Value::Map(m) = &response {
            m.iter()
                .find(|(k, _)| k == &ciborium::value::Value::Text("identifier".into()))
                .and_then(|(_, v)| {
                    if let ciborium::value::Value::Text(s) = v {
                        Some(s.clone())
                    } else {
                        None
                    }
                })
                .unwrap_or_default()
        } else {
            String::new()
        };
        // R3 belt-and-braces: empirically `runImmediately: true` does NOT
        // apply the overrides to the about:blank document Chromium opened
        // during `Target.createTarget` boot — Date.now() / Math.random()
        // still leak real wall-clock and unseeded values when called via
        // `web.evaluate` BEFORE the first user-driven navigate. The
        // `addScriptToEvaluateOnNewDocument` registration above guarantees
        // every FUTURE document gets the overrides; this Runtime.evaluate
        // applies them to the existing document context too. It is a
        // no-op for the future-document path (the next navigate's main
        // world boots fresh and the script-on-new-document handler runs
        // again). Errors are best-effort: a failed eval here is no worse
        // than today's behavior, so we log and continue.
        let eval_msg = CdpMessage {
            method: "Runtime.evaluate".to_string(),
            params: ciborium::value::Value::Map(vec![
                (
                    ciborium::value::Value::Text("expression".into()),
                    ciborium::value::Value::Text(rendered.clone()),
                ),
                (
                    ciborium::value::Value::Text("returnByValue".into()),
                    ciborium::value::Value::Bool(true),
                ),
            ]),
        };
        if let Err(e) = self.cdp.command(target_id, eval_msg, None).await {
            tracing::warn!(
                target_id,
                error = %e,
                "determinism: in-context Runtime.evaluate failed; future navigations \
                 still get the overrides via addScriptToEvaluateOnNewDocument, \
                 but the about:blank context retains real Date.now/Math.random"
            );
        }

        // animation-capture Mode C: register the deterministic IntersectionObserver
        // override so intersection-gated `whileInView` reveals fire AT MOUNT (before
        // the page's own scripts run, via runImmediately) instead of needing an
        // unreliable scroll pass. Best-effort: a failure leaves the real IO in place.
        if reveal_capture_enabled() {
            let io_msg = CdpMessage {
                method: ADD_SCRIPT_METHOD.to_string(),
                params: build_inject_params(REVEAL_IO_OVERRIDE_JS),
            };
            if let Err(e) = self.cdp.command(target_id, io_msg, None).await {
                tracing::warn!(
                    target_id,
                    error = %e,
                    "determinism: IntersectionObserver override addScript failed; \
                     whileInView reveals may capture pre-reveal"
                );
            }
        }

        // Install the deterministic, advancing virtual-time clock so client-side
        // entrance animations run to completion (faithful-entrance-animations).
        // initialVirtualTime pins the origin to the session epoch, giving a
        // single coherent, replay-stable timeline for Date.now/performance.now/
        // rAF/setTimeout. CDP-failure handling (council FND): on error, log
        // structured context and DEGRADE GRACEFULLY — the page still renders
        // (with a non-deterministic real-time clock); we never hang or hard-fail
        // the capture. Gated by LOOM_CAPTURE_VIRTUAL_TIME for rollback (P6).
        // Record whether this (determinism_enabled) session pins the virtual-time
        // clock, so the verb path can gate its budget arm+await on it. inject()
        // only runs under `determinism_enabled`; `virtual_time_enabled()` (env)
        // decides vt-clock vs frozen-clock here.
        set_clock_pinned(virtual_time_enabled());
        let mut need_freeze = !virtual_time_enabled();
        if virtual_time_enabled() {
            let vt_msg = CdpMessage {
                method: VIRTUAL_TIME_METHOD.to_string(),
                params: build_virtual_time_params(epoch_ms.0),
            };
            match self.cdp.command(target_id, vt_msg, None).await {
                Ok(_) => tracing::info!(
                    target_id,
                    epoch_ms = epoch_ms.0,
                    "determinism: virtual-time clock armed"
                ),
                Err(e) => {
                    // Recover to a DETERMINISTIC frozen clock (the prior behavior),
                    // NOT a non-deterministic real-time clock — preserving
                    // replay-equality at the cost of animations not rendering.
                    tracing::warn!(
                        target_id,
                        error = %e,
                        "determinism: setVirtualTimePolicy failed — restoring deterministic \
                         frozen clock for this target (entrance animations will not render \
                         this session, but replay determinism is preserved)"
                    );
                    need_freeze = true;
                }
            }
        } else {
            tracing::info!(
                target_id,
                "determinism: virtual time disabled (LOOM_CAPTURE_VIRTUAL_TIME=0) — \
                 frozen-clock rollback in effect"
            );
        }
        if need_freeze {
            // Install the deterministic clock-freeze on all future documents AND
            // the current context, mirroring the main inject's belt-and-braces.
            let frozen = render_clock_freeze(epoch_ms.0);
            let add_freeze = CdpMessage {
                method: ADD_SCRIPT_METHOD.to_string(),
                params: build_inject_params(&frozen),
            };
            if let Err(e) = self.cdp.command(target_id, add_freeze, None).await {
                tracing::warn!(target_id, error = %e, "determinism: clock-freeze addScript failed");
            }
            let eval_freeze = CdpMessage {
                method: "Runtime.evaluate".to_string(),
                params: ciborium::value::Value::Map(vec![
                    (
                        ciborium::value::Value::Text("expression".into()),
                        ciborium::value::Value::Text(frozen),
                    ),
                    (
                        ciborium::value::Value::Text("returnByValue".into()),
                        ciborium::value::Value::Bool(true),
                    ),
                ]),
            };
            let _ = self.cdp.command(target_id, eval_freeze, None).await;
        }

        tracing::info!(target_id, identifier = %identifier, "determinism: inject ok");
        self.per_target_identifiers
            .write()
            .insert(target_id, identifier);
        Ok(())
    }

    fn clear_target(&self, target_id: TargetId) {
        self.per_target_identifiers.write().remove(&target_id);
    }

    fn script_source_present(&self) -> bool {
        !self.script_source.is_empty()
    }
}

/// Pure helper: build the CBOR params for the
/// `Page.addScriptToEvaluateOnNewDocument` command. Used by tests +
/// `inject()`'s implementation in 5.4. Returns a ciborium::value::Value
/// with the canonical shape `{"source": <str>, "runImmediately": true}`.
pub fn build_inject_params(source: &str) -> ciborium::value::Value {
    use ciborium::value::Value;
    Value::Map(vec![
        (
            Value::Text("source".into()),
            Value::Text(source.to_string()),
        ),
        (
            Value::Text("runImmediately".into()),
            Value::Bool(RUN_IMMEDIATELY),
        ),
    ])
}

/// Pure helper: validate that the script source is the *raw template*
/// (canonical determinism markers AND the three substitution tokens).
/// Used at boot to refuse to start with a corrupted `determinism_init.js`.
/// Delegates to `determinism_script_template::is_determinism_template`.
pub fn script_source_has_determinism_markers(source: &str) -> bool {
    crate::determinism_script_template::is_determinism_template(source)
}
