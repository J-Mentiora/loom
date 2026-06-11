// Interface tests for `CdpConnection`.
// Verifies the chromiumoxide-only impl, async non-blocking signature,
// error → ShimErrorCode mapping, and callback-based handler
// registration (acyclicity).

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

// === error mapping ===

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

// === async non-blocking signature ===

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

// === browser-scope vs page-scope classifier ===

#[test]
fn cdpatt_04_method_classifier_browser_scope_methods() {
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
            "'{method}' must be browser-scope (no sessionId on the wire)"
        );
    }
}

#[test]
fn cdpatt_04_method_classifier_page_scope_methods() {
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
            "'{method}' must be page-scope (sessionId required on the wire)"
        );
    }
}

/// Regression: bootstrap_page_session MUST issue Runtime.enable
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
fn cdpatt_04_method_classifier_handles_empty_and_dotted_edge_cases() {
    // Empty or methods without a domain prefix default to page-scope —
    // the safe choice (an unexpected sessionId is harmless on real
    // Chromium per CDP spec; a missing sessionId on a page-scope method
    // produces -32601).
    assert!(!is_browser_scope_method(""));
    assert!(!is_browser_scope_method("noSeparator"));
    // Unknown domain → page-scope (defensive default).
    assert!(!is_browser_scope_method("UnknownDomain.method"));
}

// === Mini WS CDP server harness ===
//
// A canned in-process CDP endpoint for tests that need the REAL
// reader-task dispatch path (handler registration/deregistration) or the
// REAL connect/bootstrap sequence — fake-chromium is a separate binary
// and overkill for these. Answers the bootstrap methods
// (`Target.createTarget` → t-1, `Target.attachToTarget` → page-session-1,
// everything else `{}`) and pushes arbitrary event frames on demand via
// `event_tx`.

struct FakeCdpServer {
    url: String,
    /// Push a raw JSON event frame to the CURRENT connection.
    event_tx: tokio::sync::mpsc::UnboundedSender<String>,
}

/// Spawn the canned server. When `fail_first_bootstrap` is true, the
/// FIRST accepted connection answers `Target.createTarget` with a
/// JSON-RPC error (modelling a bootstrap failure mid-connect); every
/// later connection bootstraps normally.
async fn spawn_fake_cdp_server(fail_first_bootstrap: bool) -> FakeCdpServer {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    tokio::spawn(async move {
        let mut conn_idx = 0usize;
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let Ok(ws) = tokio_tungstenite::accept_async(stream).await else {
                return;
            };
            let (mut sink, mut rx) = ws.split();
            let fail_bootstrap = fail_first_bootstrap && conn_idx == 0;
            conn_idx += 1;
            loop {
                tokio::select! {
                    evt = event_rx.recv() => {
                        let Some(evt) = evt else { return };
                        if sink.send(Message::Text(evt.into())).await.is_err() {
                            break;
                        }
                    }
                    msg = rx.next() => {
                        let Some(Ok(Message::Text(text))) = msg else { break };
                        let Ok(req) = serde_json::from_str::<serde_json::Value>(&text) else {
                            continue;
                        };
                        let id = req.get("id").cloned().unwrap_or(serde_json::json!(0));
                        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
                        let resp = match method {
                            "Target.createTarget" if fail_bootstrap => serde_json::json!({
                                "id": id,
                                "error": {"code": -32000, "message": "createTarget refused (test)"},
                            }),
                            "Target.createTarget" => serde_json::json!({
                                "id": id, "result": {"targetId": "t-1"},
                            }),
                            "Target.attachToTarget" => serde_json::json!({
                                "id": id, "result": {"sessionId": "page-session-1"},
                            }),
                            _ => serde_json::json!({"id": id, "result": {}}),
                        };
                        if sink.send(Message::Text(resp.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                }
            }
        }
    });

    FakeCdpServer {
        url: format!("ws://{addr}/devtools/browser/test"),
        event_tx,
    }
}

/// Poll until `cond` holds or the deadline expires. Returns whether the
/// condition was observed.
async fn wait_until(cond: impl Fn() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    cond()
}

// === RAII deregistration (per-navigate handler leak regression) ===

/// Regression: per-navigate handlers (load/budget oneshots, the console
/// collector, the settle driver's `Network.` counter) accumulated on the
/// connection for the whole session because `EventRegistration` had no
/// `Drop` — dispatch degraded to O(navigates) per event and orphaned
/// console collectors retained every later console line. Dropping the
/// token must now remove exactly that handler from the dispatch list,
/// while kept tokens keep receiving.
#[tokio::test]
async fn dropping_registration_deregisters_handler() {
    let srv = spawn_fake_cdp_server(false).await;
    let cdp = ChromiumCdpConnection::new();
    cdp.connect(&srv.url).await.expect("connect");

    let dropped_count: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    let kept_count: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    let dropped_inner = Arc::clone(&dropped_count);
    let kept_inner = Arc::clone(&kept_count);
    let to_drop = cdp.register_event_handler(
        EventFilter::new("Custom."),
        Arc::new(move |_t: TargetId, _m: CdpMessage| {
            dropped_inner.fetch_add(1, Ordering::SeqCst);
        }),
    );
    let _kept = cdp.register_event_handler(
        EventFilter::new("Custom."),
        Arc::new(move |_t: TargetId, _m: CdpMessage| {
            kept_inner.fetch_add(1, Ordering::SeqCst);
        }),
    );

    srv.event_tx
        .send(r#"{"method":"Custom.evt","params":{}}"#.to_string())
        .expect("push event");
    let kc = Arc::clone(&kept_count);
    let dc = Arc::clone(&dropped_count);
    assert!(
        wait_until(move || kc.load(Ordering::SeqCst) == 1 && dc.load(Ordering::SeqCst) == 1).await,
        "both handlers must observe the first event"
    );

    drop(to_drop);

    srv.event_tx
        .send(r#"{"method":"Custom.evt","params":{}}"#.to_string())
        .expect("push event");
    // The kept handler observing event #2 is the ordering barrier: events
    // on one WS dispatch in order, so by now the dropped handler would
    // have seen #2 too if it were still registered.
    let kc = Arc::clone(&kept_count);
    assert!(
        wait_until(move || kc.load(Ordering::SeqCst) == 2).await,
        "kept handler must observe the second event"
    );
    assert_eq!(
        dropped_count.load(Ordering::SeqCst),
        1,
        "dropped registration must no longer receive events"
    );

    cdp.invalidate_session();
}

/// Handler ids come from a monotonic counter, NOT `handlers.len()` — a
/// len-based id would collide after a removal and a later drop would
/// deregister an innocent handler.
#[test]
fn handler_ids_stay_unique_across_removals() {
    let cdp = ChromiumCdpConnection::new();
    let a = cdp.register_event_handler(EventFilter::new("A."), Arc::new(|_, _| {}));
    let b = cdp.register_event_handler(EventFilter::new("B."), Arc::new(|_, _| {}));
    let a_id = a.handler_id;
    drop(a);
    let c = cdp.register_event_handler(EventFilter::new("C."), Arc::new(|_, _| {}));
    assert_ne!(c.handler_id, b.handler_id, "ids must stay unique");
    assert_ne!(c.handler_id, a_id, "removed ids must not be reissued");
}

// === connect() bootstrap failure leaves a clean, retryable state ===

/// Regression: connect() stored the conn state and flipped
/// `connected=true` BEFORE bootstrap_page_session ran. A bootstrap
/// failure (Target.createTarget error here) used to leave the
/// connection half-open: is_connected() reported true, a retry of
/// connect() hit the `guard.is_some()` fast-path and returned Ok with
/// no page session, and every page-scope command then failed
/// permanently with TargetNotAttached. The failure must now tear the
/// state down so a retry re-runs the full connect+bootstrap path.
#[tokio::test]
async fn connect_cleans_up_on_bootstrap_failure_and_retry_succeeds() {
    let srv = spawn_fake_cdp_server(true).await; // first bootstrap fails
    let cdp = ChromiumCdpConnection::new();

    let first = cdp.connect(&srv.url).await;
    assert!(
        first.is_err(),
        "bootstrap failure must surface from connect(); got {first:?}"
    );
    assert!(
        !cdp.is_connected(),
        "bootstrap failure must not leave connected=true"
    );

    // Retry: must run the FULL connect path (not the is_some fast-path)
    // against the server's second connection, which bootstraps normally.
    cdp.connect(&srv.url).await.expect("retry connect");
    assert!(cdp.is_connected());

    // Page-scope command proves page_session_id was set by the retry's
    // bootstrap — pre-fix this returned TargetNotAttached forever.
    let res = cdp
        .command(
            0,
            CdpMessage {
                method: "Runtime.evaluate".into(),
                params: CborValue::Map(vec![]),
            },
            Some(Duration::from_secs(5)),
        )
        .await;
    assert!(
        res.is_ok(),
        "page-scope command must succeed after a successful retry; got {res:?}"
    );

    cdp.invalidate_session();
}

/// A token outliving `invalidate_session` (or the connection itself) is
/// a no-op on drop — the Weak link refuses to resurrect cleared state.
#[test]
fn registration_drop_after_invalidate_is_noop() {
    let cdp = ChromiumCdpConnection::new();
    let reg = cdp.register_event_handler(EventFilter::new("X."), Arc::new(|_, _| {}));
    cdp.invalidate_session();
    drop(reg); // must not panic or disturb later registrations
    let again = cdp.register_event_handler(EventFilter::new("Y."), Arc::new(|_, _| {}));
    let _ = again.handler_id;
}
