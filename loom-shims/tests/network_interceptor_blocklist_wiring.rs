// Behavior tests for ChromiumNetworkInterceptor blocklist wiring.
//
// AC coverage:
//  .1     : subscribe() issues Fetch.enable when blocklist
//                     is non-empty, no-op when empty
//   blocked sub-resource drains as a BlockedEvent
//                     with reason from the section header
//   top-frame Document request is skipped (operator
//                     opt-in to the primary URL)
//   Regression      : legacy `new()` constructor stays no-op for
//                     Fetch.enable (back-compat with pre-feature)

use ciborium::value::Value;
use loom_shims::cdp_connection::cdp_connection::{
    CdpConnection, CdpError, EventFilter, EventHandler, EventRegistration,
};
use loom_shims::ipc_endpoint::ipc_endpoint::{CdpMessage, TargetId};
use loom_shims::network_interceptor::network_interceptor::{
    ChromiumNetworkInterceptor, NetworkInterceptor,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Mock CdpConnection that records every `command()` call AND captures
/// the registered event handler so tests can fire synthetic events.
struct RecordingCdp {
    calls: Mutex<Vec<(TargetId, String, Value)>>,
    fetch_handler: Mutex<Option<EventHandler>>,
}

impl RecordingCdp {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            fetch_handler: Mutex::new(None),
        })
    }

    fn recorded_methods(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|(_, m, _)| m.clone())
            .collect()
    }

    /// Trigger the captured Fetch.* handler with a synthetic event.
    fn fire_fetch_event(&self, target_id: TargetId, method: &str, params: Value) {
        let h = self.fetch_handler.lock().unwrap().clone();
        if let Some(h) = h {
            h(
                target_id,
                CdpMessage {
                    method: method.to_string(),
                    params,
                },
            );
        }
    }
}

#[async_trait::async_trait]
impl CdpConnection for RecordingCdp {
    async fn connect(&self, _ws_url: &str) -> Result<(), CdpError> {
        Ok(())
    }

    async fn command(
        &self,
        target_id: TargetId,
        msg: CdpMessage,
        _timeout: Option<Duration>,
    ) -> Result<Value, CdpError> {
        self.calls
            .lock()
            .unwrap()
            .push((target_id, msg.method, msg.params));
        Ok(Value::Map(vec![]))
    }

    fn register_event_handler(
        &self,
        filter: EventFilter,
        handler: EventHandler,
    ) -> EventRegistration {
        // Capture the Fetch.* handler so tests can fire synthetic
        // Fetch.requestPaused events through it. (The Network.* handler
        // is registered first by the constructor and is irrelevant to
        // these tests.)
        if filter.method_prefix.starts_with("Fetch") {
            *self.fetch_handler.lock().unwrap() = Some(handler);
        }
        EventRegistration::detached(0)
    }

    fn invalidate_session(&self) {}

    fn is_connected(&self) -> bool {
        true
    }
}

/// Construct a synthetic `Fetch.requestPaused` params CBOR value.
fn paused_params(request_id: &str, url: &str, frame_id: &str, resource_type: &str) -> Value {
    Value::Map(vec![
        (
            Value::Text("requestId".into()),
            Value::Text(request_id.into()),
        ),
        (
            Value::Text("request".into()),
            Value::Map(vec![(Value::Text("url".into()), Value::Text(url.into()))]),
        ),
        (Value::Text("frameId".into()), Value::Text(frame_id.into())),
        (
            Value::Text("resourceType".into()),
            Value::Text(resource_type.into()),
        ),
    ])
}

/// Default analytics-only blocklist used by most tests.
fn ga_blocklist() -> Vec<(String, String)> {
    vec![(
        "analytics".to_string(),
        "*.google-analytics.com".to_string(),
    )]
}

// === subscribe() issues Fetch.enable when blocklist is non-empty ===

#[tokio::test]
async fn test_subscribe_issues_fetch_enable_when_blocklist_nonempty() {
    let cdp = RecordingCdp::new();
    let interceptor = ChromiumNetworkInterceptor::new_with_blocklist(cdp.clone(), ga_blocklist());

    interceptor
        .subscribe(0)
        .await
        .expect("subscribe should succeed");

    let methods = cdp.recorded_methods();
    assert!(
        methods.contains(&"Fetch.enable".to_string()),
        "subscribe() must issue Fetch.enable when blocklist is non-empty; recorded = {methods:?}"
    );
}

#[tokio::test]
async fn test_subscribe_no_op_when_blocklist_empty() {
    let cdp = RecordingCdp::new();
    let interceptor = ChromiumNetworkInterceptor::new_with_blocklist(cdp.clone(), Vec::new());

    interceptor
        .subscribe(0)
        .await
        .expect("subscribe should succeed (and be a no-op)");

    let methods = cdp.recorded_methods();
    assert!(
        !methods.contains(&"Fetch.enable".to_string()),
        "subscribe() must NOT issue Fetch.enable when blocklist is empty; recorded = {methods:?}"
    );
}

/// Regression — the legacy `new()` constructor (back-compat alias) must
/// NOT install the Fetch.* handler. A future regression that wired it
/// up unconditionally would break callers that don't want enforcement.
#[tokio::test]
async fn test_legacy_new_constructor_is_no_op_for_fetch_enable() {
    let cdp = RecordingCdp::new();
    let interceptor = ChromiumNetworkInterceptor::new(cdp.clone());

    interceptor
        .subscribe(0)
        .await
        .expect("subscribe via legacy new() must be a no-op success");

    let methods = cdp.recorded_methods();
    assert!(
        methods.is_empty() || !methods.contains(&"Fetch.enable".to_string()),
        "legacy new() must not lead to Fetch.enable; recorded = {methods:?}"
    );
    // And the Fetch.* handler must NOT have been registered (so synthetic
    // Fetch events would be silently dropped — pre-feature behavior).
    assert!(
        cdp.fetch_handler.lock().unwrap().is_none(),
        "legacy new() must not register a Fetch.* event handler"
    );
}

// === blocklisted sub-resource drains as BlockedEvent ===

#[tokio::test]
async fn test_request_paused_blocklisted_calls_fail_request_and_records() {
    let cdp = RecordingCdp::new();
    let interceptor = ChromiumNetworkInterceptor::new_with_blocklist(cdp.clone(), ga_blocklist());
    interceptor.subscribe(0).await.expect("subscribe");

    // Synthetic sub-resource request matching the blocklist.
    cdp.fire_fetch_event(
        0,
        "Fetch.requestPaused",
        paused_params(
            "fetch-1",
            "https://www.google-analytics.com/analytics.js",
            "frame-X",
            "Script",
        ),
    );

    // The handler spawns a tokio task to send Fetch.failRequest.
    // Yield long enough for it to run.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let methods = cdp.recorded_methods();
    assert!(
        methods.contains(&"Fetch.failRequest".to_string()),
        "blocklisted URL must trigger Fetch.failRequest; recorded = {methods:?}"
    );
    assert!(
        !methods.contains(&"Fetch.continueRequest".to_string()),
        "blocklisted URL must NOT trigger Fetch.continueRequest; recorded = {methods:?}"
    );

    let blocked = interceptor.drain_blocked(0);
    assert_eq!(
        blocked.len(),
        1,
        "exactly one BlockedEvent recorded; got {blocked:?}"
    );
    assert_eq!(
        blocked[0].url,
        "https://www.google-analytics.com/analytics.js"
    );
    assert_eq!(blocked[0].reason, "analytics", "reason from section header");
    assert_eq!(blocked[0].matched_pattern, "*.google-analytics.com");
}

#[tokio::test]
async fn test_request_paused_non_blocklisted_calls_continue() {
    let cdp = RecordingCdp::new();
    let interceptor = ChromiumNetworkInterceptor::new_with_blocklist(cdp.clone(), ga_blocklist());
    interceptor.subscribe(0).await.expect("subscribe");

    cdp.fire_fetch_event(
        0,
        "Fetch.requestPaused",
        paused_params(
            "fetch-2",
            "https://example.com/style.css",
            "frame-X",
            "Stylesheet",
        ),
    );

    tokio::time::sleep(Duration::from_millis(20)).await;

    let methods = cdp.recorded_methods();
    assert!(
        methods.contains(&"Fetch.continueRequest".to_string()),
        "non-blocklisted URL must trigger Fetch.continueRequest; recorded = {methods:?}"
    );
    assert!(
        !methods.contains(&"Fetch.failRequest".to_string()),
        "non-blocklisted URL must NOT trigger Fetch.failRequest"
    );
    assert!(
        interceptor.drain_blocked(0).is_empty(),
        "no BlockedEvents for non-blocklisted URL"
    );
}

// === top-frame Document request is skipped ===

#[tokio::test]
async fn test_top_frame_document_skips_gate() {
    let cdp = RecordingCdp::new();
    let interceptor = ChromiumNetworkInterceptor::new_with_blocklist(cdp.clone(), ga_blocklist());
    interceptor.subscribe(0).await.expect("subscribe");

    // Synthetic top-frame Document request to a URL that WOULD match
    // the blocklist if it were a sub-resource. The skip-gate must let
    // it through (operator opt-in to the page-level navigate).
    cdp.fire_fetch_event(
        0,
        "Fetch.requestPaused",
        paused_params(
            "fetch-doc-1",
            "https://www.google-analytics.com/dashboard",
            "frame-MAIN",
            "Document",
        ),
    );

    tokio::time::sleep(Duration::from_millis(20)).await;

    let methods = cdp.recorded_methods();
    assert!(
        methods.contains(&"Fetch.continueRequest".to_string()),
        "top-frame Document MUST continue regardless of blocklist match; recorded = {methods:?}"
    );
    assert!(
        !methods.contains(&"Fetch.failRequest".to_string()),
        "top-frame Document MUST NOT fail; recorded = {methods:?}"
    );
    assert!(
        interceptor.drain_blocked(0).is_empty(),
        "no BlockedEvents for top-frame Document"
    );
}

/// Regression (audit: 'top_frame_seen skip-gate mismodels Chromium frame
/// identity'): in real Chromium the MAIN frame's frameId is stable for
/// the tab's lifetime — the second and every later operator-issued
/// Page.navigate pauses a Document on the SAME frameId as the first.
/// The old seen-set exempted only the first Document per (target,
/// frame), so navigating a session twice to a blocklisted host blocked
/// the second navigation (ERR_BLOCKED_BY_CLIENT). EVERY main-frame
/// Document must be skip-gated, even when its URL matches the
/// blocklist.
#[tokio::test]
async fn test_second_main_frame_document_is_skip_gated() {
    let cdp = RecordingCdp::new();
    let interceptor = ChromiumNetworkInterceptor::new_with_blocklist(cdp.clone(), ga_blocklist());
    interceptor.subscribe(0).await.expect("subscribe");

    // First Document for frame-MAIN — establishes the main-frame
    // identity and is skip-gated (operator's primary URL).
    cdp.fire_fetch_event(
        0,
        "Fetch.requestPaused",
        paused_params(
            "fetch-doc-1",
            "https://example.com/index.html",
            "frame-MAIN",
            "Document",
        ),
    );
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Second Document for frame-MAIN — a SECOND operator navigate of
    // the same tab, this time to a blocklisted host. Must ALSO be
    // skip-gated (same stable main-frame frameId).
    cdp.fire_fetch_event(
        0,
        "Fetch.requestPaused",
        paused_params(
            "fetch-doc-2",
            "https://www.google-analytics.com/dashboard",
            "frame-MAIN",
            "Document",
        ),
    );
    tokio::time::sleep(Duration::from_millis(20)).await;

    let methods = cdp.recorded_methods();
    let n_continue = methods
        .iter()
        .filter(|m| *m == "Fetch.continueRequest")
        .count();
    assert_eq!(
        n_continue, 2,
        "both main-frame Documents must continue; recorded = {methods:?}"
    );
    assert!(
        !methods.contains(&"Fetch.failRequest".to_string()),
        "a main-frame Document must never be failed; recorded = {methods:?}"
    );
    assert!(
        interceptor.drain_blocked(0).is_empty(),
        "no BlockedEvents for main-frame Documents"
    );
}

/// An iframe's Document arrives on its OWN frameId (distinct from the
/// main frame) and IS gated normally — a blocklisted iframe document is
/// failed + recorded, and the main frame stays exempt afterwards.
#[tokio::test]
async fn test_iframe_document_on_distinct_frame_is_gated() {
    let cdp = RecordingCdp::new();
    let interceptor = ChromiumNetworkInterceptor::new_with_blocklist(cdp.clone(), ga_blocklist());
    interceptor.subscribe(0).await.expect("subscribe");

    // Main-frame Document first (establishes main-frame identity).
    cdp.fire_fetch_event(
        0,
        "Fetch.requestPaused",
        paused_params(
            "fetch-doc-1",
            "https://example.com/index.html",
            "frame-MAIN",
            "Document",
        ),
    );
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Iframe Document on a DIFFERENT frameId, blocklisted URL → gated.
    cdp.fire_fetch_event(
        0,
        "Fetch.requestPaused",
        paused_params(
            "fetch-doc-iframe",
            "https://www.google-analytics.com/iframe.html",
            "frame-IFRAME",
            "Document",
        ),
    );
    tokio::time::sleep(Duration::from_millis(10)).await;

    // Another main-frame Document afterwards — still skip-gated (the
    // iframe must not have stolen or reset the main-frame identity).
    cdp.fire_fetch_event(
        0,
        "Fetch.requestPaused",
        paused_params(
            "fetch-doc-2",
            "https://example.com/next.html",
            "frame-MAIN",
            "Document",
        ),
    );
    tokio::time::sleep(Duration::from_millis(20)).await;

    let methods = cdp.recorded_methods();
    let n_continue = methods
        .iter()
        .filter(|m| *m == "Fetch.continueRequest")
        .count();
    let n_fail = methods.iter().filter(|m| *m == "Fetch.failRequest").count();
    assert_eq!(
        n_continue, 2,
        "both main-frame Documents continued; recorded = {methods:?}"
    );
    assert_eq!(
        n_fail, 1,
        "blocklisted iframe Document gated and failed; recorded = {methods:?}"
    );

    let blocked = interceptor.drain_blocked(0);
    assert_eq!(blocked.len(), 1, "iframe Document recorded as BlockedEvent");
    assert_eq!(
        blocked[0].url,
        "https://www.google-analytics.com/iframe.html"
    );
}
