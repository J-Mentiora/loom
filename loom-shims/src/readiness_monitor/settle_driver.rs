// SettleDriver — the CDP I/O layer that feeds the pure [`ReadinessMachine`].
//
// It owns the side of the readiness wait that touches the browser: it counts
// in-flight requests from `Network.*` events (excluding long-lived
// WebSocket/EventSource streams), polls `document.readyState` + `location.href`
// + a DOM-mutation counter via a single `Runtime.evaluate` per tick, builds a
// [`PageObservation`], and steps the machine. The loop paces with a small await
// to avoid busy-spin (practitioner R-b), but the VERDICT depends only on the
// per-tick observation sequence + tick count — never elapsed wall-time — so a
// recorded session replays to the identical outcome (DET-CORE).
//
// One bounded exception: an outer wall-clock guard caps the WHOLE wait at the
// caller's budget (D-INTAKE: never hangs). It can only fire on pages that
// never settle (where loom already accepts bounded divergence); pages that
// settle reach their verdict from the tick sequence alone.

use super::readiness_monitor::{
    PageObservation, ReadinessMachine, SettleConfig, SettleMode, SettleOutcome,
};
use crate::cdp_connection::cdp_connection::{CdpConnection, EventFilter, EventHandler};
use crate::ipc_endpoint::ipc_endpoint::{CdpMessage, TargetId};
use ciborium::value::Value as CborValue;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

/// Pacing between ticks. Bounded await so the loop does not hot-spin; its
/// DURATION never enters the verdict (which counts ticks, not ms).
const TICK_PACING: Duration = Duration::from_millis(5);

/// Cap on the per-tick probe's CDP timeout. The tick ceiling assumes a tick
/// costs ~`TICK_PACING`; handing each probe the FULL caller budget would let a
/// wedged renderer (Runtime.evaluate stalling to its timeout while the hang
/// watchdog's Browser.getVersion pings still succeed) stretch the wait to
/// ceiling × budget — hours. One second per probe keeps a single slow tick
/// bounded without flapping on routine busy-main-thread pauses.
const MAX_PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// Derive a [`SettleConfig`] whose tick ceiling bounds the wait to roughly the
/// caller's `timeout` (ceiling = timeout / pacing). The ceiling is a fixed tick
/// count derived from a recorded input (the budget), so it is replay-stable;
/// the wall-clock pacing only affects how fast ticks elapse, never the verdict.
pub fn config_for_timeout(timeout: Duration) -> SettleConfig {
    let ceiling = (timeout.as_millis() / TICK_PACING.as_millis().max(1)) as u32;
    SettleConfig::with_ceiling(ceiling)
}

/// The single JS probe run each tick. Self-installs a `MutationObserver` on
/// first call (counting childList/characterData/attribute mutations — the
/// "meaningful mutation" set, R-g), then returns
/// `[readyState==complete, location.href, activity-since-last-call]` as a JSON
/// string. Reading the mutation counter resets it, so each tick sees only the
/// mutations of that interval.
///
/// `activity` = mutations + currently-running animations
/// (`document.getAnimations()` with `playState==='running'`). Folding the
/// running-animation count into the same "is the page still changing?" signal
/// (faithful-entrance-animations) keeps the readiness gate open until
/// animations finish — covering WAAPI `element.animate()` and compositor-thread
/// CSS transforms/opacity, which animate without mutating the DOM and so are
/// invisible to the MutationObserver. JS-rAF (framer-motion) reveals are already
/// caught via the inline-style mutations they make; this adds the rest. When the
/// page is truly quiet (no mutations AND no running animations) `activity` is 0
/// and the existing quiet-tick machine settles.
const SETTLE_PROBE_JS: &str = r#"(function(){
  if(!window.__loomSettleObs){
    window.__loomSettleMut = 0;
    try {
      var cb = function(muts){ window.__loomSettleMut += muts.length; };
      window.__loomSettleObs = new MutationObserver(cb);
      window.__loomSettleObs.observe(document.documentElement || document, {childList:true, subtree:true, characterData:true, attributes:true});
    } catch(e) { window.__loomSettleObs = null; }
  }
  var m = window.__loomSettleMut|0; window.__loomSettleMut = 0;
  var anim = 0;
  try {
    if (typeof document.getAnimations === 'function') {
      anim = document.getAnimations().filter(function(a){ return a.playState === 'running'; }).length;
    }
  } catch(e) { anim = 0; }
  return JSON.stringify([document.readyState === 'complete', location.href || '', (m + anim)|0]);
})()"#;

/// Outcome of a readiness wait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettleResult {
    pub mode: SettleMode,
    pub outcome: SettleOutcome,
    /// Number of ticks (poll iterations) the wait took. Mapped to `settle_ms`
    /// host-side; never enters `outcome_hash`.
    pub ticks: u32,
    /// In-flight (non-WS/SSE) request count at the final tick.
    pub network_count: u32,
    /// The page began a top-level navigation it initiated itself (the loaded
    /// document went, or started, `loading` when it should have been
    /// `complete`) — e.g. `window.location` assign / `<meta refresh>` /
    /// form-POST. The caller re-attaches: re-arms the virtual-time budget for
    /// the new document and re-runs the settle gate. Shim-INTERNAL — derived
    /// purely from the (deterministic) tick observation sequence and NEVER
    /// enters `outcome_hash`, so replay byte-equality is unaffected.
    pub renavigated: bool,
}

/// Run a readiness wait against `target_id` for `mode`, bounded by `config`.
/// `load_fired` reports whether `Page.loadEventFired` has already been seen by
/// the caller (the navigate path subscribes to it before issuing the navigate).
///
/// `already_loaded` reports whether the page was settled BEFORE this wait began
/// (the standalone `wait_for` path, where the caller asserts the document is
/// already complete). It seeds the unsolicited-renavigation detector: when the
/// page is `already_loaded` (or once a `complete` tick is observed), a
/// subsequent `loading` observation means the page started a top-level
/// navigation it initiated itself → return early with `renavigated=true` so the
/// caller can re-attach. `navigate` passes `already_loaded=false` (its first
/// settle ticks may transiently read `loading` before the freshly-loaded
/// document reflects `complete`); `wait_for` passes `true` (a `loading` read on
/// an asserted-loaded page is anomalous and means the page renavigated).
pub async fn wait_for_settle(
    cdp: &Arc<dyn CdpConnection>,
    target_id: TargetId,
    mode: SettleMode,
    config: SettleConfig,
    load_fired: bool,
    already_loaded: bool,
    cdp_timeout: Duration,
) -> SettleResult {
    // In-flight request set: requestId → present means an open, counted
    // request. WebSocket/EventSource are never inserted, so they never count
    // (they are long-lived by design and would otherwise block idle forever).
    let inflight: Arc<parking_lot::Mutex<HashSet<String>>> =
        Arc::new(parking_lot::Mutex::new(HashSet::new()));

    let inflight_h = inflight.clone();
    let net_handler: EventHandler = Arc::new(move |_t: TargetId, msg: CdpMessage| {
        match msg.method.as_str() {
            "Network.requestWillBeSent" => {
                // Skip long-lived EventSource streams (WebSocket uses a
                // separate webSocketCreated event we never subscribe to).
                if cdp_str(&msg.params, "type").as_deref() == Some("EventSource") {
                    return;
                }
                if let Some(id) = cdp_str(&msg.params, "requestId") {
                    inflight_h.lock().insert(id);
                }
            }
            "Network.loadingFinished"
            | "Network.loadingFailed"
            | "Network.requestServedFromCache" => {
                if let Some(id) = cdp_str(&msg.params, "requestId") {
                    inflight_h.lock().remove(&id);
                }
            }
            _ => {}
        }
    });
    let _net_reg = cdp.register_event_handler(EventFilter::new("Network."), net_handler);

    let mut machine = ReadinessMachine::new(mode, config);
    let mut last_href: Option<String> = None;
    // Unsolicited-renavigation detector (client-nav-reattach): the document is
    // EXPECTED complete once the caller asserted it loaded (`already_loaded`) or
    // once we observe a `complete` tick. A later `loading` observation then
    // means the page began a top-level navigation it initiated itself, and the
    // caller must re-attach to the new document.
    let mut expected_complete = already_loaded;

    // Outer wall-clock bound (D-INTAKE: never hangs). The tick ceiling bounds
    // the VERDICT deterministically, but each tick costs pacing + a CDP
    // round-trip, so slow probes could otherwise overrun the caller's budget
    // by a large factor. Healthy pages settle well before either bound, so the
    // verdict for pages that settle stays a pure function of the tick
    // observation sequence (DET-CORE); only never-settling pages can observe
    // this guard — the same bounded-determinism tradeoff as the virtual-time
    // budget warn path in `page_navigate`.
    let deadline = tokio::time::Instant::now() + cdp_timeout;
    let probe_timeout = cdp_timeout.min(MAX_PROBE_TIMEOUT);

    loop {
        // Poll page state for this tick.
        let (ready_complete, href, dom_mutations) = probe_page(cdp, target_id, probe_timeout).await;
        let in_flight = inflight.lock().len() as u32;

        // Unsolicited top-level renavigation: the page was (or was asserted)
        // complete, and is now `loading` again — a new in-flight document. Bail
        // so the caller re-arms the virtual-time budget and re-settles on it.
        // The `timeout` outcome is a placeholder the caller replaces on a
        // successful re-attach; if the caller exhausts its hop/deadline budget
        // mid-renavigation it is the correct terminal verdict (the page never
        // settled within budget).
        if expected_complete && !ready_complete {
            return SettleResult {
                mode,
                outcome: SettleOutcome::Timeout,
                ticks: machine.ticks(),
                network_count: in_flight,
                renavigated: true,
            };
        }
        if ready_complete {
            expected_complete = true;
        }

        // URL is "stable" this tick if it has not changed since the previous
        // observation (client-side redirects have quiesced). The first tick is
        // treated as not-yet-stable so a redirect that fires immediately after
        // load is not mistaken for a stable URL.
        let url_stable = match &last_href {
            Some(prev) => prev == &href,
            None => false,
        };
        last_href = Some(href);

        let obs = PageObservation {
            load_fired,
            in_flight,
            ready_complete,
            url_stable,
            dom_mutations,
        };

        if let Some(outcome) = machine.step(obs) {
            return SettleResult {
                mode,
                outcome,
                ticks: machine.ticks(),
                network_count: in_flight,
                renavigated: false,
            };
        }

        // Wall-clock expiry: classify with the same terminal rule the tick
        // ceiling uses so `dom_unstable` stays distinguishable from `timeout`
        // regardless of which bound fires first.
        if tokio::time::Instant::now() >= deadline {
            return SettleResult {
                mode,
                outcome: machine.timeout_verdict(obs),
                ticks: machine.ticks(),
                network_count: in_flight,
                renavigated: false,
            };
        }

        tokio::time::sleep(TICK_PACING).await;
    }
}

/// Run the settle probe once. On any CDP error returns a conservative
/// observation (`ready=false`, empty href, 0 mutations) — a transient probe
/// failure should not be read as "settled".
async fn probe_page(
    cdp: &Arc<dyn CdpConnection>,
    target_id: TargetId,
    cdp_timeout: Duration,
) -> (bool, String, u32) {
    let msg = CdpMessage {
        method: "Runtime.evaluate".into(),
        params: CborValue::Map(vec![
            (
                CborValue::Text("expression".into()),
                CborValue::Text(SETTLE_PROBE_JS.into()),
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
    match cdp.command(target_id, msg, Some(cdp_timeout)).await {
        Ok(r) => parse_probe(&r).unwrap_or((false, String::new(), 0)),
        Err(_) => (false, String::new(), 0),
    }
}

/// Pull the string at `key` out of a CDP params CBOR map.
fn cdp_str(params: &CborValue, key: &str) -> Option<String> {
    if let CborValue::Map(entries) = params {
        for (k, v) in entries {
            if let (CborValue::Text(k), CborValue::Text(v)) = (k, v) {
                if k == key {
                    return Some(v.clone());
                }
            }
        }
    }
    None
}

/// Parse the `Runtime.evaluate` response for the settle probe — `result.value`
/// is the JSON string `[bool, "href", mutations]`.
fn parse_probe(resp: &CborValue) -> Option<(bool, String, u32)> {
    // Navigate to result.value (a Text holding JSON).
    let value_text = cbor_get(resp, "result")
        .and_then(|r| cbor_get(r, "value"))
        .and_then(|v| match v {
            CborValue::Text(s) => Some(s.clone()),
            _ => None,
        })?;
    let arr: serde_json::Value = serde_json::from_str(&value_text).ok()?;
    let ready = arr.get(0).and_then(|v| v.as_bool()).unwrap_or(false);
    let href = arr
        .get(1)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let muts = arr.get(2).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    Some((ready, href, muts))
}

fn cbor_get<'a>(v: &'a CborValue, key: &str) -> Option<&'a CborValue> {
    if let CborValue::Map(entries) = v {
        for (k, val) in entries {
            if let CborValue::Text(k) = k {
                if k == key {
                    return Some(val);
                }
            }
        }
    }
    None
}
