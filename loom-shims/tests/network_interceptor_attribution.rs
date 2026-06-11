// Behavior tests for ChromiumNetworkInterceptor main-document
// attribution + navigate-START event clearing.
//
// Coverage:
//   - `Network.loadingFailed` (which carries no frameId/loaderId of its
//     own) is backfilled from the Document `Network.requestWillBeSent`
//     correlation map by requestId, so iframe document failures stay
//     attributable to their frame and cannot fail the whole navigate.
//   - `clear_events` drops pending hashed events (the navigate-START
//     reset that prevents a failed prior navigate from leaking stale
//     Document events into the next receipt) without touching the
//     blocked-events audit accumulator.

use ciborium::value::Value;
use loom_shims::cdp_connection::cdp_connection::{
    CdpConnection, CdpError, EventFilter, EventHandler, EventRegistration,
};
use loom_shims::ipc_endpoint::ipc_endpoint::{CdpMessage, TargetId};
use loom_shims::network_interceptor::network_interceptor::{
    ChromiumNetworkInterceptor, LoomNetworkEvent, NetworkInterceptor,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Mock CdpConnection that captures the registered `Network.*` event
/// handler so tests can fire synthetic CDP events through the real
/// interceptor pipeline (parse → backfill → append).
struct NetworkHandlerCdp {
    network_handler: Mutex<Option<EventHandler>>,
}

impl NetworkHandlerCdp {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            network_handler: Mutex::new(None),
        })
    }

    fn fire(&self, target_id: TargetId, method: &str, params: Value) {
        let h = self.network_handler.lock().unwrap().clone();
        let h = h.expect("Network.* handler must be registered by the constructor");
        h(
            target_id,
            CdpMessage {
                method: method.to_string(),
                params,
            },
        );
    }
}

#[async_trait::async_trait]
impl CdpConnection for NetworkHandlerCdp {
    async fn connect(&self, _ws_url: &str) -> Result<(), CdpError> {
        Ok(())
    }

    async fn command(
        &self,
        _target_id: TargetId,
        _msg: CdpMessage,
        _timeout: Option<Duration>,
    ) -> Result<Value, CdpError> {
        Ok(Value::Map(vec![]))
    }

    fn register_event_handler(
        &self,
        filter: EventFilter,
        handler: EventHandler,
    ) -> EventRegistration {
        if filter.method_prefix.starts_with("Network") {
            *self.network_handler.lock().unwrap() = Some(handler);
        }
        EventRegistration { handler_id: 0 }
    }

    fn invalidate_session(&self) {}

    fn is_connected(&self) -> bool {
        true
    }
}

fn text(s: &str) -> Value {
    Value::Text(s.into())
}

fn request_will_be_sent_params(
    request_id: &str,
    frame_id: &str,
    loader_id: &str,
    url: &str,
) -> Value {
    Value::Map(vec![
        (text("requestId"), text(request_id)),
        (text("frameId"), text(frame_id)),
        (text("loaderId"), text(loader_id)),
        (text("type"), text("Document")),
        (
            text("request"),
            Value::Map(vec![
                (text("url"), text(url)),
                (text("method"), text("GET")),
            ]),
        ),
    ])
}

fn response_received_params(
    request_id: &str,
    frame_id: &str,
    loader_id: &str,
    url: &str,
    status: u16,
) -> Value {
    Value::Map(vec![
        (text("requestId"), text(request_id)),
        (text("frameId"), text(frame_id)),
        (text("loaderId"), text(loader_id)),
        (text("type"), text("Document")),
        (
            text("response"),
            Value::Map(vec![
                (text("url"), text(url)),
                (text("status"), Value::Integer(i64::from(status).into())),
                (text("mimeType"), text("text/html")),
            ]),
        ),
    ])
}

fn loading_failed_params(request_id: &str, error_text: &str, canceled: bool) -> Value {
    Value::Map(vec![
        (text("requestId"), text(request_id)),
        (text("type"), text("Document")),
        (text("errorText"), text(error_text)),
        (text("canceled"), Value::Bool(canceled)),
    ])
}

// === loadingFailed attribution is backfilled from requestWillBeSent ===

#[test]
fn loading_failed_attribution_backfilled_by_request_id() {
    let cdp = NetworkHandlerCdp::new();
    let interceptor = ChromiumNetworkInterceptor::new(cdp.clone());

    cdp.fire(
        7,
        "Network.requestWillBeSent",
        request_will_be_sent_params("R-if-1", "frame-iframe", "loader-iframe", "http://x/embed"),
    );
    cdp.fire(
        7,
        "Network.loadingFailed",
        loading_failed_params("R-if-1", "net::ERR_BLOCKED_BY_CLIENT", false),
    );

    let drained = interceptor.drain_events_attributed(7);
    assert_eq!(drained.len(), 1);
    let (event, attribution) = &drained[0];
    assert_eq!(
        event.error_reason.as_deref(),
        Some("net::ERR_BLOCKED_BY_CLIENT")
    );
    assert_eq!(
        attribution.frame_id, "frame-iframe",
        "frameId must be backfilled from the Document requestWillBeSent"
    );
    assert_eq!(attribution.loader_id, "loader-iframe");
}

#[test]
fn loading_failed_without_prior_request_stays_unattributed() {
    let cdp = NetworkHandlerCdp::new();
    let interceptor = ChromiumNetworkInterceptor::new(cdp.clone());

    cdp.fire(
        7,
        "Network.loadingFailed",
        loading_failed_params("R-unknown", "net::ERR_CONNECTION_REFUSED", false),
    );

    let drained = interceptor.drain_events_attributed(7);
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].1.frame_id, "");
    assert_eq!(drained[0].1.loader_id, "");
}

#[test]
fn cancelled_loading_failed_is_dropped_by_the_pipeline() {
    let cdp = NetworkHandlerCdp::new();
    let interceptor = ChromiumNetworkInterceptor::new(cdp.clone());

    // A JS-redirect-cancelled prior document load (net::ERR_ABORTED,
    // canceled=true) must never reach the hashed accumulator.
    cdp.fire(
        7,
        "Network.loadingFailed",
        loading_failed_params("R-prior", "net::ERR_ABORTED", true),
    );

    assert!(interceptor.drain_events_attributed(7).is_empty());
}

#[test]
fn response_received_attribution_passes_through_unchanged() {
    let cdp = NetworkHandlerCdp::new();
    let interceptor = ChromiumNetworkInterceptor::new(cdp.clone());

    cdp.fire(
        7,
        "Network.responseReceived",
        response_received_params("R-main", "frame-main", "loader-main", "http://x/", 200),
    );

    let drained = interceptor.drain_events_attributed(7);
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].0.status, 200);
    assert_eq!(drained[0].1.frame_id, "frame-main");
    assert_eq!(drained[0].1.loader_id, "loader-main");
}

// === clear_events drops stale hashed events (navigate-START reset) ===

#[test]
fn clear_events_drops_pending_hashed_events() {
    let cdp = NetworkHandlerCdp::new();
    let interceptor = ChromiumNetworkInterceptor::new(cdp.clone());

    cdp.fire(
        7,
        "Network.responseReceived",
        response_received_params("R-stale", "frame-main", "loader-old", "http://x/old", 404),
    );

    interceptor.clear_events(7);

    assert!(
        interceptor.drain_events_attributed(7).is_empty(),
        "stale Document events must not survive the navigate-START reset"
    );
}

#[test]
fn clear_events_keeps_attribution_map_for_late_failures() {
    let cdp = NetworkHandlerCdp::new();
    let interceptor = ChromiumNetworkInterceptor::new(cdp.clone());

    cdp.fire(
        7,
        "Network.requestWillBeSent",
        request_will_be_sent_params("R-prior", "frame-main", "loader-old", "http://x/old"),
    );
    interceptor.clear_events(7);

    // A late (non-cancelled) failure of the superseded load arriving
    // AFTER the next navigate started must stay attributable to its
    // ORIGINAL loader, so loader matching can exclude it from the new
    // navigation's main-document verdict.
    cdp.fire(
        7,
        "Network.loadingFailed",
        loading_failed_params("R-prior", "net::ERR_FAILED", false),
    );

    let drained = interceptor.drain_events_attributed(7);
    assert_eq!(drained.len(), 1);
    assert_eq!(
        drained[0].1.loader_id, "loader-old",
        "correlation map must survive clear_events"
    );
}

#[test]
fn append_without_attribution_drains_with_default_attribution() {
    let cdp = NetworkHandlerCdp::new();
    let interceptor = ChromiumNetworkInterceptor::new(cdp);

    interceptor.append(
        3,
        LoomNetworkEvent {
            method: "GET".into(),
            url: "http://x/".into(),
            request_hash: String::new(),
            response_hash: String::new(),
            status: 200,
            content_type: String::new(),
            duration_ms: 0,
            response_bytes: 0,
            error_reason: None,
            error_kind: None,
        },
    );

    let drained = interceptor.drain_events_attributed(3);
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].1, Default::default());
}
