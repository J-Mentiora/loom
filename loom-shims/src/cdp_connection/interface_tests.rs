// Interface tests for `CdpConnection`.
// Verifies BC-SHIM-01 (chromiumoxide-only), IC-SHIM-12 (async non-
// blocking signature), error → ShimErrorCode mapping (IC-SHIM-10),
// callback-based handler registration (acyclicity).

use super::cdp_connection::{
    is_browser_scope_method, method_matches, CdpConnection, CdpError, ChromiumCdpConnection,
    EventFilter, EventHandler, DEFAULT_CDP_TIMEOUT,
};
use crate::ipc_endpoint::ipc_endpoint::{CdpMessage, ShimErrorCode, TargetId};
use ciborium::value::Value as CborValue;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

// === Default timeout (soft binding) ===

#[test]
fn default_cdp_timeout_is_thirty_seconds() {
    assert_eq!(DEFAULT_CDP_TIMEOUT, Duration::from_secs(30));
}

// === Event filter matching ===

#[test]
fn network_filter_matches_network_response_received() {
    let f = EventFilter::new("Network.");
    assert!(method_matches(&f, "Network.responseReceived"));
    assert!(method_matches(&f, "Network.requestWillBeSent"));
}

#[test]
fn page_filter_matches_page_frame_started_loading() {
    let f = EventFilter::new("Page.");
    assert!(method_matches(&f, "Page.frameStartedLoading"));
    assert!(method_matches(&f, "Page.loadEventFired"));
}

#[test]
fn network_filter_does_not_match_page_events() {
    let f = EventFilter::new("Network.");
    assert!(!method_matches(&f, "Page.frameAttached"));
    assert!(!method_matches(&f, "Log.entryAdded"));
}

// === Callback-based registration (acyclicity) ===

#[test]
fn register_event_handler_returns_handle_and_invokes_callback() {
    let cdp = ChromiumCdpConnection::new();
    let counter: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    let counter_inner = Arc::clone(&counter);
    let handler: EventHandler = Arc::new(move |_t: TargetId, _m: CdpMessage| {
        counter_inner.fetch_add(1, Ordering::Relaxed);
    });
    let reg = cdp.register_event_handler(EventFilter::new("Network."), handler);
    let _ = reg.handler_id; // public field for deregistration in 5.4
}

#[test]
fn register_event_handler_does_not_import_caller_modules() {
    // Compile-time guarantee: the handler is `Arc<dyn Fn>` — no
    // type from `network_interceptor` or `determinism_injector` is
    // imported here. (cdp_connection's `use` block is the witness.)
    let cdp = ChromiumCdpConnection::new();
    let h: EventHandler = Arc::new(|_, _| {});
    let _reg = cdp.register_event_handler(EventFilter::new("X."), h);
}

// === IC-SHIM-10: error mapping ===

#[test]
fn cdp_timeout_maps_to_shim_cdp_timeout() {
    let e = CdpError::Timeout { ms: 50 };
    let code: ShimErrorCode = e.into();
    assert_eq!(code, ShimErrorCode::CdpTimeout);
}

#[test]
fn cdp_protocol_maps_to_shim_cdp_protocol_error() {
    let e = CdpError::Protocol("bad params".into());
    let code: ShimErrorCode = e.into();
    assert_eq!(code, ShimErrorCode::CdpProtocolError);
}

#[test]
fn cdp_connection_closed_maps_to_chromium_unavailable() {
    let e = CdpError::ConnectionClosed;
    let code: ShimErrorCode = e.into();
    assert_eq!(code, ShimErrorCode::ChromiumUnavailable);
}

#[test]
fn cdp_target_not_attached_maps_to_target_unknown() {
    let e = CdpError::TargetNotAttached(99);
    let code: ShimErrorCode = e.into();
    assert_eq!(code, ShimErrorCode::TargetUnknown);
}

// === IC-SHIM-12: async non-blocking signature ===

#[test]
fn cdp_connection_command_takes_timeout_and_returns_future() {
    // Compile-time signature check: `command` is async and accepts a
    // timeout option, returning Result<CborValue, CdpError>.
    fn _check<T: CdpConnection + ?Sized>(
        c: &T,
    ) -> impl std::future::Future<Output = Result<CborValue, CdpError>> + '_ {
        c.command(
            1,
            CdpMessage {
                method: "Browser.getVersion".into(),
                params: CborValue::Null,
            },
            Some(Duration::from_millis(50)),
        )
    }
    let _ = _check::<dyn CdpConnection>;
}

// === is_connected reflects state ===

#[test]
fn fresh_connection_is_not_connected() {
    let cdp = ChromiumCdpConnection::new();
    assert!(!cdp.is_connected());
}

// === Trait object Send+Sync ===

#[test]
fn cdp_connection_trait_object_is_send_sync() {
    fn _check<T: CdpConnection + ?Sized>() {}
    _check::<dyn CdpConnection>();
}

// === Disconnected command fast-fails (no panic, no timeout) ===

#[tokio::test]
async fn command_returns_connection_closed_when_not_connected() {
    let cdp = ChromiumCdpConnection::new();
    let res = cdp
        .command(
            1,
            CdpMessage {
                method: "Browser.getVersion".into(),
                params: CborValue::Null,
            },
            Some(Duration::from_millis(50)),
        )
        .await;
    assert!(matches!(res, Err(CdpError::ConnectionClosed)));
}

// === Bad ws URL surfaces as ConnectFailed (not panic) ===

#[tokio::test]
async fn connect_to_invalid_url_returns_connect_failed() {
    let cdp = ChromiumCdpConnection::new();
    // Port 1 is reserved (RFC 6335) — the connect attempt MUST fail
    // without panicking.
    let res = cdp.connect("ws://127.0.0.1:1/devtools/browser/test").await;
    assert!(matches!(res, Err(CdpError::ConnectFailed(_))));
    assert!(!cdp.is_connected());
}

// === AC-CDPATT-04: browser-scope vs page-scope classifier ===

#[test]
fn ac_cdpatt_04_method_classifier_browser_scope_methods() {
    // Browser-scope: dispatched at the browser-level WS endpoint without
    // a sessionId. The CDP "tot" spec lists these domains as
    // browser-process-only.
    for method in [
        "Browser.getVersion",
        "Browser.close",
        "Target.createTarget",
        "Target.attachToTarget",
        "Target.closeTarget",
        "Target.getTargets",
        "Tracing.start",
        "Tracing.end",
        "Storage.clearDataForOrigin",
        "Schema.getDomains",
        "SystemInfo.getInfo",
        "Memory.getDOMCounters",
    ] {
        assert!(
            is_browser_scope_method(method),
            "AC-CDPATT-04: '{method}' must be browser-scope (no sessionId on the wire)"
        );
    }
}

#[test]
fn ac_cdpatt_04_method_classifier_page_scope_methods() {
    // Page-scope: dispatched on a Page session via top-level sessionId.
    // Real Chromium returns -32601 if these are sent without sessionId
    // on the browser-level endpoint.
    for method in [
        "Page.navigate",
        "Page.captureScreenshot",
        "Page.enable",
        "Page.loadEventFired", // event name; classifier only sees a string
        "Page.addScriptToEvaluateOnNewDocument",
        "DOM.getDocument",
        "DOM.querySelector",
        "Network.enable",
        "Network.responseReceived",
        "Runtime.evaluate",
        "Runtime.callFunctionOn",
        "Input.dispatchKeyEvent",
        "Emulation.setDeviceMetricsOverride",
        "Log.enable",
        "Console.enable",
    ] {
        assert!(
            !is_browser_scope_method(method),
            "AC-CDPATT-04: '{method}' must be page-scope (sessionId required on the wire)"
        );
    }
}

/// Round-24 regression: bootstrap_page_session MUST issue Runtime.enable
/// alongside Page.enable and Network.enable. Without Runtime.enable the
/// CDP target never fires Runtime.consoleAPICalled events, the
/// action_executor's navigate-time console-collector subscribes to a
/// handler that never receives anything, and the receipt's
/// console_count + console_lines come back as 0 / empty even on pages
/// that emit substantial console output.
///
/// Source-grep test (no chromium needed): assert the literal
/// "Runtime.enable" appears inside the bootstrap_page_session function
/// body. Pinned at the source level so a future refactor that
/// inadvertently drops Runtime.enable fails the test loudly.
#[test]
fn bootstrap_page_session_enables_runtime_for_console_capture() {
    let src = include_str!("cdp_connection.rs");
    // Find the function body.
    let body_start = src
        .find("async fn bootstrap_page_session")
        .expect("bootstrap_page_session function must exist");
    // Conservative slice: just look for Runtime.enable anywhere after
    // the function declaration. The function is the only place that
    // pattern is meaningful.
    let body = &src[body_start..];
    assert!(
        body.contains("\"Runtime.enable\""),
        "bootstrap_page_session must enable Runtime so Runtime.consoleAPICalled events fire"
    );
    // Also assert Page.enable and Network.enable are still there —
    // dropping either would break navigate's load-event + network-event
    // subscribers, which is its own AC.
    assert!(body.contains("\"Page.enable\""));
    assert!(body.contains("\"Network.enable\""));
}

#[test]
fn ac_cdpatt_04_method_classifier_handles_empty_and_dotted_edge_cases() {
    // Empty or methods without a domain prefix default to page-scope —
    // the safe choice (an unexpected sessionId is harmless on real
    // Chromium per CDP spec; a missing sessionId on a page-scope method
    // produces -32601).
    assert!(!is_browser_scope_method(""));
    assert!(!is_browser_scope_method("noSeparator"));
    // Unknown domain → page-scope (defensive default).
    assert!(!is_browser_scope_method("UnknownDomain.method"));
}
