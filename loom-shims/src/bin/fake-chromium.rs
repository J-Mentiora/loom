//! `fake-chromium` — test-only binary that simulates a real Chromium
//! Chrome DevTools Protocol endpoint for integration tests.
//!
//! Behaviour:
//!   1. Bind a tokio-tungstenite WebSocket server on 127.0.0.1:0.
//!   2. Wait for the kernel-assigned port to be ready.
//!   3. Print `"DevTools listening on ws://127.0.0.1:<port>/..."` to
//!      stderr (matching real Chromium's startup line).
//!   4. Also write `<port>\n<path>` to `<user_data_dir>/DevToolsActivePort`
//!      if `LOOM_FAKE_CHROMIUM_USER_DATA_DIR` is set (matches Chromium's
//!      file-based discovery path).
//!   5. Accept WebSocket connections and respond to canned CDP methods.
//!   6. Exit cleanly on SIGTERM / Ctrl-C.
//!
//! Optional env vars:
//! - `LOOM_FAKE_CHROMIUM_USER_DATA_DIR` — write `DevToolsActivePort` here.
//! - `LOOM_FAKE_CHROMIUM_LOG` — append a JSON line per received method
//!   (used by integration tests to assert what the daemon sent).
//! - `LOOM_FAKE_CHROMIUM_FAIL_AFTER_N` — close WS after N requests
//!   (used to test surface_unavailable).
//! - `LOOM_FAKE_CHROMIUM_FIXTURE` — path to a JSON file containing a
//!   tiny DOM model used to answer `DOM.querySelector`, `DOM.getBoxModel`,
//!   `DOM.scrollIntoViewIfNeeded`, and `Page.getLayoutMetrics`. Shape:
//!   `{ "boxes": { "<selector>": [x1, y1, x2, y2] }, "viewport": [w, h] }`.
//!   Used by the Click/Hover/Scroll hit-test integration tests.
//! - `LOOM_FAKE_CHROMIUM_SCRIPT` — path to a JSON file driving the
//!   settle-capture readiness probe deterministically across ticks. Shape:
//!   `{ "settle_probe": [[ready_complete, "href", dom_mutations], ...],
//!      "perpetual_inflight": N }`.
//!   The i-th settle probe `Runtime.evaluate` (the one carrying
//!   `__loomSettleMut`) returns `settle_probe[i]` (the last entry repeats
//!   once exhausted), letting a test script a client-side redirect
//!   (href changes then stabilises), async-after-load content (a late
//!   DOM-mutation burst), or a never-settling DOM (perpetual mutations).
//!   `perpetual_inflight` pins N never-finishing in-flight requests
//!   (re-asserted on every probe so the wait sees them regardless of when
//!   the host's Network handler registered) → drives the bounded-timeout
//!   path. The settle-capture never-settles / redirect e2e cases use this.

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use tokio::net::TcpListener;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    // STEP 1: bind first.
    let listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("fake-chromium: bind failed: {e}");
            return std::process::ExitCode::from(2);
        }
    };
    let addr = listener.local_addr().unwrap();
    let ws_path = "/devtools/browser/fake-chromium-test";

    // STEP 4: write DevToolsActivePort file (real Chromium does this).
    if let Ok(udd) = std::env::var("LOOM_FAKE_CHROMIUM_USER_DATA_DIR") {
        let path = PathBuf::from(&udd).join("DevToolsActivePort");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, format!("{}\n{}", addr.port(), ws_path));
    }

    // STEP 3: print the canonical startup line.
    eprintln!(
        "DevTools listening on ws://127.0.0.1:{}{}",
        addr.port(),
        ws_path
    );
    let _ = std::io::stderr().flush();

    let log_path: Option<PathBuf> = std::env::var("LOOM_FAKE_CHROMIUM_LOG")
        .ok()
        .map(PathBuf::from);
    let fail_after: Option<usize> = std::env::var("LOOM_FAKE_CHROMIUM_FAIL_AFTER_N")
        .ok()
        .and_then(|s| s.parse().ok());

    // STEP 5+6: accept loop, racing against shutdown signal.
    tokio::select! {
        _ = accept_loop(listener, log_path, fail_after) => {
            // accept_loop only returns on listener error
            std::process::ExitCode::SUCCESS
        }
        _ = tokio::signal::ctrl_c() => {
            std::process::ExitCode::SUCCESS
        }
    }
}

async fn accept_loop(listener: TcpListener, log_path: Option<PathBuf>, fail_after: Option<usize>) {
    loop {
        let (stream, _peer) = match listener.accept().await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("fake-chromium: accept failed: {e}");
                return;
            }
        };
        let log = log_path.clone();
        tokio::spawn(async move {
            handle_connection(stream, log, fail_after).await;
        });
    }
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    log_path: Option<PathBuf>,
    fail_after: Option<usize>,
) {
    // When LOOM_FAKE_CHROMIUM_REQUIRE_SESSION_ID=1, validate
    // that every page-scope CDP request carries a top-level `sessionId`
    // field. Real Chromium rejects page-scope methods without sessionId
    // with `{code:-32601, message:"'<method>' wasn't found"}`; this flag
    // makes the integration test enforce the same constraint.
    let require_session_id =
        std::env::var("LOOM_FAKE_CHROMIUM_REQUIRE_SESSION_ID").as_deref() == Ok("1");

    let ws = match accept_async(stream).await {
        Ok(w) => w,
        Err(e) => {
            eprintln!("fake-chromium: WS handshake failed: {e}");
            return;
        }
    };
    let (mut write, mut read) = ws.split();
    let mut request_count = 0usize;
    // Real Chromium only delivers `Fetch.requestPaused` events AFTER the
    // host issues `Fetch.enable`. Mirror that here so the synthetic
    // PageWithTracker emission below doesn't fire when the host has
    // disabled the blocklist gate (`blocklist_enabled = false` →
    // `subscribe()` skipped → no `Fetch.enable`).
    let mut fetch_enabled = false;
    // settle-capture: per-connection cursor into LOOM_FAKE_CHROMIUM_SCRIPT's
    // `settle_probe` array. Reset on each Page.navigate so every navigation
    // replays the script from the top; advanced once per settle probe.
    let script = settle_script();
    let mut settle_idx: usize = 0;

    while let Some(msg) = read.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                eprintln!("fake-chromium: WS read error: {e}");
                return;
            }
        };
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => return,
            Message::Ping(p) => {
                let _ = write.send(Message::Pong(p)).await;
                continue;
            }
            _ => continue,
        };

        let value: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("fake-chromium: JSON decode: {e}");
                continue;
            }
        };

        // Append to log file if requested.
        if let Some(p) = &log_path {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
            {
                let _ = writeln!(f, "{}", value);
            }
        }

        request_count += 1;

        // Optional crash injection.
        if let Some(n) = fail_after {
            if request_count > n {
                eprintln!("fake-chromium: closing WS after {n} requests");
                let _ = write.send(Message::Close(None)).await;
                return;
            }
        }

        let id = value.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
        let method = value
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let params = value.get("params").cloned().unwrap_or(Value::Null);
        let session_id = value
            .get("sessionId")
            .and_then(|v| v.as_str())
            .map(String::from);

        // Enforcement: page-scope methods MUST carry sessionId.
        if require_session_id && session_id.is_none() && !is_browser_scope_method_local(&method) {
            let err = json!({
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("'{method}' wasn't found"),
                },
            });
            let _ = write.send(Message::Text(err.to_string().into())).await;
            continue;
        }

        // For Page.navigate, derive a per-URL response shape so integration
        // tests can drive the receipt across success / 4xx / 5xx /
        // transport-error branches without touching real Chromium.
        // Conventions:
        //   http://fake.test/status/<N>  → emit Network.responseReceived
        //                                   with type=Document, status=N
        //   http://fake.test/error/<CDP> → set errorText=<CDP> in the
        //                                   Page.navigate response AND emit
        //                                   Network.loadingFailed with
        //                                   type=Document, errorText=<CDP>
        //   anything else                → bare canned response (legacy)
        let nav_url_pattern = if method == "Page.navigate" {
            params
                .get("url")
                .and_then(|v| v.as_str())
                .map(parse_fake_url_pattern)
                .unwrap_or(FakeUrlPattern::None)
        } else {
            FakeUrlPattern::None
        };

        // Page.navigate resets the settle-script cursor so each navigation
        // replays LOOM_FAKE_CHROMIUM_SCRIPT from the top.
        if method == "Page.navigate" {
            settle_idx = 0;
        }

        // Runtime.evaluate is driven by an expression-pattern convention
        // (parallels Page.navigate's URL-pattern scheme above) so
        // integration tests can drive every evaluate-result branch
        // synthetically. See `parse_fake_evaluate_pattern` for the
        // sentinel grammar. The settle-capture readiness probe (carrying the
        // `__loomSettleMut` global) is special-cased here because its
        // response advances per-connection script state.
        let expression = if method == "Runtime.evaluate" {
            params
                .get("expression")
                .and_then(|v| v.as_str())
                .map(String::from)
        } else {
            None
        };
        let is_settle_probe = expression
            .as_deref()
            .map(|e| e.contains("__loomSettleMut"))
            .unwrap_or(false);
        let evaluate_response = if let Some(expr) = &expression {
            if is_settle_probe {
                let resp = script.probe_response(settle_idx);
                settle_idx += 1;
                Some(resp)
            } else {
                Some(build_fake_evaluate_response(expr))
            }
        } else {
            None
        };

        // Track Fetch domain enable state so the PageWithTracker branch
        // below can mirror real Chromium's "events only after Fetch.enable"
        // semantics.
        if method == "Fetch.enable" {
            fetch_enabled = true;
        } else if method == "Fetch.disable" {
            fetch_enabled = false;
        }

        let mut result = if let Some(eval_result) = evaluate_response {
            eval_result
        } else {
            canned_response(&method, &params)
        };
        if method == "Page.navigate" {
            if let FakeUrlPattern::Error(ref code) = nav_url_pattern {
                result["errorText"] = json!(code);
            }
        }

        // CDP error sentinel: `canned_response` may return
        // `{"__cdp_error__": {"code": ..., "message": ...}}` to signal
        // that this method should respond with a JSON-RPC error envelope
        // instead of `{result: ...}`. Used by `DOM.getBoxModel` on
        // hidden / zero-area / unknown nodes.
        let mut response = if let Some(err) = result.get("__cdp_error__").cloned() {
            json!({ "id": id, "error": err })
        } else {
            json!({ "id": id, "result": result })
        };
        if let Some(sid) = &session_id {
            response["sessionId"] = json!(sid);
        }
        let response_text = response.to_string();
        if write
            .send(Message::Text(response_text.into()))
            .await
            .is_err()
        {
            return;
        }

        // settle-capture never-settles (network) shape: re-assert N
        // never-finishing in-flight requests on every settle probe. Stable
        // requestIds make the inserts idempotent in the host's in-flight set,
        // so the count stays pinned at N regardless of when the host's
        // `Network.` handler registered (it registers only once the settle
        // wait begins, after this navigate's load fires). N > the idle
        // threshold keeps `networkidle` from ever quiescing → bounded Timeout.
        if is_settle_probe && script.perpetual_inflight > 0 {
            for i in 0..script.perpetual_inflight {
                let mut evt = json!({
                    "method": "Network.requestWillBeSent",
                    "params": {
                        "requestId": format!("perpetual-{i}"),
                        "type": "Fetch",
                        "request": { "url": "http://fake.test/poll", "method": "GET" },
                    },
                });
                if let Some(sid) = &session_id {
                    evt["sessionId"] = json!(sid);
                }
                if write
                    .send(Message::Text(evt.to_string().into()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }

        // Per-URL synthetic CDP events emitted right after Page.navigate
        // resolves. Order matches what real Chromium produces:
        // Network.* events fire before Page.loadEventFired.
        if method == "Page.navigate" {
            let nav_url = params
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            match &nav_url_pattern {
                FakeUrlPattern::Status(status) => {
                    // Document requestWillBeSent FIRST (real Chromium order) so
                    // the full-capture accumulator records the HTTP method —
                    // responseReceived alone has no method.
                    let mut doc_req = json!({
                        "method": "Network.requestWillBeSent",
                        "params": {
                            "requestId": "fake-req-1",
                            "loaderId": "fake-loader-1",
                            "timestamp": 1.0,
                            "wallTime": 1_700_000_000.0,
                            "type": "Document",
                            "request": { "url": nav_url, "method": "GET" },
                        },
                    });
                    if let Some(sid) = &session_id {
                        doc_req["sessionId"] = json!(sid);
                    }
                    let _ = write.send(Message::Text(doc_req.to_string().into())).await;

                    let mut evt = json!({
                        "method": "Network.responseReceived",
                        "params": {
                            "requestId": "fake-req-1",
                            "loaderId": "fake-loader-1",
                            "timestamp": 1.0,
                            "type": "Document",
                            "response": {
                                "url": nav_url,
                                "status": status,
                                "statusText": "",
                                "mimeType": "text/html",
                            },
                        },
                    });
                    if let Some(sid) = &session_id {
                        evt["sessionId"] = json!(sid);
                    }
                    let _ = write.send(Message::Text(evt.to_string().into())).await;

                    // A known xhr to `/api/thing` — exercises the full-capture
                    // network-entries path (NON-Document, with method+status+
                    // resource_type) that the studio's route footprints need.
                    // Dropped by the Document-only `network_events` path, so it
                    // appears ONLY in `network_entries`.
                    let api_url = format!("{}/api/thing", nav_url.trim_end_matches('/'));
                    let mut xhr_req = json!({
                        "method": "Network.requestWillBeSent",
                        "params": {
                            "requestId": "fake-xhr-1",
                            "loaderId": "fake-loader-1",
                            "timestamp": 1.1,
                            "wallTime": 1_700_000_001.0,
                            "type": "XHR",
                            "request": { "url": api_url, "method": "GET" },
                        },
                    });
                    if let Some(sid) = &session_id {
                        xhr_req["sessionId"] = json!(sid);
                    }
                    let _ = write.send(Message::Text(xhr_req.to_string().into())).await;

                    let mut xhr_resp = json!({
                        "method": "Network.responseReceived",
                        "params": {
                            "requestId": "fake-xhr-1",
                            "loaderId": "fake-loader-1",
                            "timestamp": 1.2,
                            "type": "XHR",
                            "response": {
                                "url": api_url,
                                "status": 200,
                                "statusText": "OK",
                                "mimeType": "application/json",
                            },
                        },
                    });
                    if let Some(sid) = &session_id {
                        xhr_resp["sessionId"] = json!(sid);
                    }
                    let _ = write.send(Message::Text(xhr_resp.to_string().into())).await;
                }
                FakeUrlPattern::Error(code) => {
                    let mut evt = json!({
                        "method": "Network.loadingFailed",
                        "params": {
                            "requestId": "fake-req-1",
                            "timestamp": 1.0,
                            "type": "Document",
                            "errorText": code,
                            "canceled": false,
                        },
                    });
                    if let Some(sid) = &session_id {
                        evt["sessionId"] = json!(sid);
                    }
                    let _ = write.send(Message::Text(evt.to_string().into())).await;
                }
                FakeUrlPattern::PageWithTracker => {
                    // Emit two `Fetch.requestPaused`
                    // events synthetically, mirroring chromium's CDP
                    // wire shape. ONLY when the host has issued
                    // `Fetch.enable` (real Chromium gates emission the
                    // same way; the `blocklist_enabled = false` path
                    // never sends `Fetch.enable`, so no Fetch events).
                    // The first carries the operator's primary URL with
                    // `resourceType=Document` → interceptor's frameId-
                    // based skip-gate lets it through. The second is a
                    // sub-resource on a blocklisted host
                    // (`*.google-analytics.com`) → interceptor must
                    // answer `Fetch.failRequest{ errorReason:
                    // "BlockedByClient"}` and record one BlockedEvent.
                    if fetch_enabled {
                        let mut doc_evt = json!({
                            "method": "Fetch.requestPaused",
                            "params": {
                                "requestId": "fake-fetch-doc-1",
                                "request": { "url": &nav_url, "method": "GET" },
                                "frameId": "fake-frame-1",
                                "resourceType": "Document"
                            }
                        });
                        if let Some(sid) = &session_id {
                            doc_evt["sessionId"] = json!(sid);
                        }
                        let _ = write.send(Message::Text(doc_evt.to_string().into())).await;

                        let ga_url = "https://www.google-analytics.com/analytics.js";
                        let mut ga_evt = json!({
                            "method": "Fetch.requestPaused",
                            "params": {
                                "requestId": "fake-fetch-ga-1",
                                "request": { "url": ga_url, "method": "GET" },
                                "frameId": "fake-frame-1",
                                "resourceType": "Script"
                            }
                        });
                        if let Some(sid) = &session_id {
                            ga_evt["sessionId"] = json!(sid);
                        }
                        let _ = write.send(Message::Text(ga_evt.to_string().into())).await;
                    }

                    // Also emit the document's Network.responseReceived
                    // so the action_executor's status_code derivation
                    // succeeds (mirrors the Status branch behavior).
                    let mut resp_evt = json!({
                        "method": "Network.responseReceived",
                        "params": {
                            "requestId": "fake-req-1",
                            "loaderId": "fake-loader-1",
                            "timestamp": 1.0,
                            "type": "Document",
                            "response": {
                                "url": &nav_url,
                                "status": 200,
                                "statusText": "OK",
                                "mimeType": "text/html",
                            },
                        },
                    });
                    if let Some(sid) = &session_id {
                        resp_evt["sessionId"] = json!(sid);
                    }
                    let _ = write.send(Message::Text(resp_evt.to_string().into())).await;
                }
                FakeUrlPattern::None => {}
            }

            // Emit Page.loadEventFired after Page.navigate so the daemon's
            // wait-for-load-event doesn't time out. Real Chromium emits
            // this event on the page session's sessionId, not the
            // browser one.
            let mut evt = json!({
                "method": "Page.loadEventFired",
                "params": { "timestamp": 1.0 }
            });
            if let Some(sid) = &session_id {
                evt["sessionId"] = json!(sid);
            }
            let _ = write.send(Message::Text(evt.to_string().into())).await;
        }
    }
}

/// URL-driven test scaffolding for navigate-error and blocklist behaviours.
/// Real Chromium would fetch the URL from the network; fake-chromium
/// pattern-matches the URL string and emits the corresponding CDP events
/// synthetically.
enum FakeUrlPattern {
    /// `http://fake.test/status/<N>` → emit Network.responseReceived with
    /// the parsed HTTP status.
    Status(u16),
    /// `http://fake.test/error/<CDP>` → emit Network.loadingFailed AND
    /// set errorText=<CDP> in the Page.navigate response.
    Error(String),
    /// `http://fake.test/page-with-tracker` → after
    /// Page.navigate, emit `Fetch.requestPaused` events for the
    /// document AND for a hard-coded analytics sub-resource
    /// (`https://www.google-analytics.com/analytics.js`). The document
    /// event has `resourceType="Document"` and is first per-frame so
    /// the interceptor's frameId-based skip-gate lets it through; the
    /// sub-resource event has `resourceType="Script"` and matches the
    /// default blocklist → must be answered with `Fetch.failRequest`.
    PageWithTracker,
    /// Anything else — emit no synthetic Network event (status_code
    /// will remain 0 from the shim's perspective, mirroring real
    /// Chromium with caching disabled).
    None,
}

/// Build a synthetic `Runtime.evaluate` response body from a test-only
/// expression sentinel. The fake-chromium does NOT execute JS — it
/// pattern-matches the expression string and constructs the same shape
/// real Chromium would return for the equivalent successful or thrown
/// evaluation.
///
/// Sentinels (all start with `__loom_test_`):
///   `__loom_test_int__`              → result.value = 2 (integer)
///   `__loom_test_str__`              → result.value = "hello"
///   `__loom_test_doc_title__`        → result.value = "My Page"
///   `__loom_test_null__`             → result.value = null
///   `__loom_test_undef__`            → result.type = "undefined", no value
///   `__loom_test_empty_str__`        → result.value = ""
///   `__loom_test_pi__`               → result.value = 3.141592653589793 (float)
///   `__loom_test_obj__`              → result.value = {"label":"Click here","count":42}
///   `__loom_test_throw__:<MSG>`      → exceptionDetails with description Error: <MSG>
///   `__loom_test_large__:<SIZE_KB>`  → result.value = "x" repeated SIZE_KB×1024 times
///
/// Anything else → empty `{}` (caller treats as a no-op evaluate).
fn build_fake_evaluate_response(expression: &str) -> Value {
    // settle-capture: the ReadinessMonitor settle probe (identified by its
    // unique `__loomSettleMut` global) is intercepted in `handle_connection`
    // BEFORE this function is reached, because its per-tick response is driven
    // by mutable per-connection state (the script index) + the optional
    // `LOOM_FAKE_CHROMIUM_SCRIPT`. See `SettleScript::probe_response`.

    // Throw sentinel:  __loom_test_throw__:<message>
    if let Some(msg) = expression.strip_prefix("__loom_test_throw__:") {
        return json!({
            "result": {
                "type": "object",
                "subtype": "error",
                "className": "Error",
            },
            "exceptionDetails": {
                "exceptionId": 1,
                "text": "Uncaught",
                "lineNumber": 0,
                "columnNumber": 6,
                "scriptId": "1",
                "exception": {
                    "type": "object",
                    "subtype": "error",
                    "className": "Error",
                    "description": format!("Error: {msg}"),
                },
            },
        });
    }

    // Large sentinel:  __loom_test_large__:<size_kb>
    if let Some(rest) = expression.strip_prefix("__loom_test_large__:") {
        let kb: usize = rest.parse().unwrap_or(80);
        // Build a string of EXACT length `kb * 1024 - 2` so the
        // canonical-JSON encoding (which adds two surrounding quotes)
        // is exactly `kb * 1024` bytes. This lets boundary tests target
        // 65_535 / 65_536 / 65_537 byte canonical-JSON outputs precisely.
        let target_chars = kb.saturating_mul(1024).saturating_sub(2);
        let big = "x".repeat(target_chars);
        return json!({
            "result": {
                "type": "string",
                "value": big,
            },
        });
    }

    // Bytes-target sentinel:  __loom_test_size__:<bytes>
    // Produces canonical-JSON of exactly <bytes> length (string value
    // wrapped in two quote chars, so the inner string is bytes-2 chars).
    if let Some(rest) = expression.strip_prefix("__loom_test_size__:") {
        let bytes: usize = rest.parse().unwrap_or(65_536);
        let target_chars = bytes.saturating_sub(2);
        let big = "x".repeat(target_chars);
        return json!({
            "result": {
                "type": "string",
                "value": big,
            },
        });
    }

    match expression {
        "__loom_test_int__" => json!({
            "result": { "type": "number", "value": 2 },
        }),
        "__loom_test_str__" => json!({
            "result": { "type": "string", "value": "hello" },
        }),
        "__loom_test_doc_title__" => json!({
            "result": { "type": "string", "value": "My Page" },
        }),
        "__loom_test_null__" => json!({
            "result": { "type": "object", "subtype": "null", "value": Value::Null },
        }),
        "__loom_test_undef__" => json!({
            // CDP returns no `value` field for undefined.
            "result": { "type": "undefined" },
        }),
        "__loom_test_empty_str__" => json!({
            "result": { "type": "string", "value": "" },
        }),
        "__loom_test_pi__" => {
            // Real Chromium returns floats verbatim in the value field.
            // Use std::f64::consts::PI to satisfy clippy::approx_constant.
            json!({
                "result": { "type": "number", "value": std::f64::consts::PI },
            })
        }
        "__loom_test_obj__" => json!({
            "result": {
                "type": "object",
                "value": { "label": "Click here", "count": 42 },
            },
        }),
        _ => json!({}),
    }
}

fn parse_fake_url_pattern(url: &str) -> FakeUrlPattern {
    if let Some(rest) = url
        .strip_prefix("http://fake.test/status/")
        .or_else(|| url.strip_prefix("https://fake.test/status/"))
    {
        let n: &str = rest.split('?').next().unwrap_or("");
        if let Ok(status) = n.parse::<u16>() {
            return FakeUrlPattern::Status(status);
        }
    }
    if let Some(rest) = url
        .strip_prefix("http://fake.test/error/")
        .or_else(|| url.strip_prefix("https://fake.test/error/"))
    {
        let code = rest.split('?').next().unwrap_or("");
        if !code.is_empty() {
            return FakeUrlPattern::Error(code.to_string());
        }
    }
    if url == "http://fake.test/page-with-tracker" || url == "https://fake.test/page-with-tracker" {
        return FakeUrlPattern::PageWithTracker;
    }
    FakeUrlPattern::None
}

/// Local copy of the browser-scope classifier so fake-chromium doesn't
/// need to depend on loom-shims' library code (it's a separate bin
/// target). Stays in sync with `is_browser_scope_method` in
/// `loom-shims/src/cdp_connection/interfaces.rs`.
fn is_browser_scope_method_local(method: &str) -> bool {
    matches!(
        method.split('.').next().unwrap_or(""),
        "Browser" | "Target" | "Tracing" | "Storage" | "Schema" | "SystemInfo" | "Memory"
    )
}

/// Canned CDP-method responses. Best-effort — unknown methods get an
/// empty `{}` result so the daemon doesn't error out on routine calls.
///
/// `params` is consumed for `DOM.querySelector` (extract `selector`) and
/// `DOM.getBoxModel` (extract `nodeId`); for other methods it is ignored.
/// A per-process, per-call ephemeral frame id — stands in for the random
/// per-navigation `frameId` real Chromium embeds in `DOM.getDocument`. Distinct
/// across independent runs so a content-stable `dom_snapshot_hash` can only hold
/// if the shim strips it.
fn ephemeral_frame_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    format!(
        "fake-frame-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

fn canned_response(method: &str, params: &Value) -> Value {
    match method {
        "Page.navigate" => json!({
            "frameId": "fake-frame-1",
            "loaderId": "fake-loader-1"
        }),
        "DOM.getDocument" => {
            // Honor `pierce`: real Chromium inlines shadow-DOM + iframe
            // contentDocument subtrees only when pierce:true. Each inlined document
            // carries its OWN ephemeral frameId. Node ids are STABLE synthetic
            // values; only the frameIds vary per call, so two captures of the same
            // tree normalize (frameId stripped recursively) to identical bytes —
            // which the pierced-path determinism e2e asserts.
            //
            // This fixture validates the normalization plumbing ONLY. It does NOT
            // reproduce browser-enforced same-origin / CORS isolation that real
            // Chromium applies to pierced subtrees.
            let pierce = params
                .get("pierce")
                .and_then(|p| p.as_bool())
                .unwrap_or(false);
            if pierce {
                json!({
                    "root": {
                        "nodeId": 1,
                        "backendNodeId": 1,
                        "nodeName": "#document",
                        "nodeType": 9,
                        "childNodeCount": 2,
                        "frameId": ephemeral_frame_id(),
                        "children": [
                            // Shadow host — its shadowRoot subtree is inlined under pierce.
                            {
                                "nodeId": 2,
                                "backendNodeId": 2,
                                "nodeName": "DIV",
                                "nodeType": 1,
                                "shadowRoots": [
                                    {
                                        "nodeId": 3,
                                        "backendNodeId": 3,
                                        "nodeName": "#document-fragment",
                                        "nodeType": 11,
                                        "children": [
                                            {
                                                "nodeId": 4,
                                                "backendNodeId": 4,
                                                "nodeName": "SPAN",
                                                "nodeType": 1,
                                                "children": []
                                            }
                                        ]
                                    }
                                ],
                                "children": []
                            },
                            // Iframe — its contentDocument is inlined under pierce,
                            // each level carrying its own ephemeral frameId.
                            {
                                "nodeId": 5,
                                "backendNodeId": 5,
                                "nodeName": "IFRAME",
                                "nodeType": 1,
                                "frameId": ephemeral_frame_id(),
                                "contentDocument": {
                                    "nodeId": 6,
                                    "backendNodeId": 6,
                                    "nodeName": "#document",
                                    "nodeType": 9,
                                    "frameId": ephemeral_frame_id(),
                                    "children": [
                                        {
                                            "nodeId": 7,
                                            "backendNodeId": 7,
                                            "nodeName": "BODY",
                                            "nodeType": 1,
                                            "children": []
                                        }
                                    ]
                                }
                            }
                        ]
                    }
                })
            } else {
                json!({
                    "root": {
                        "nodeId": 1,
                        "backendNodeId": 1,
                        "nodeName": "#document",
                        "nodeType": 9,
                        "childNodeCount": 0,
                        // Ephemeral per-navigation frame id, mirroring real Chromium.
                        // Varies per call + per process so the determinism e2e proves
                        // `dom_snapshot_hash` normalization STRIPS it: two independent
                        // same-seed runs hash identically ONLY because the shim removes
                        // this id (see loom_shared::dom_normalize).
                        "frameId": ephemeral_frame_id(),
                        "children": []
                    }
                })
            }
        }
        "DOM.querySelector" => {
            let sel = params
                .get("selector")
                .and_then(|s| s.as_str())
                .unwrap_or("");
            let node_id = dom_fixture().ids_by_selector.get(sel).copied().unwrap_or(0);
            json!({ "nodeId": node_id })
        }
        "DOM.scrollIntoViewIfNeeded" => json!({}),
        "DOM.getBoxModel" => {
            let nid = params.get("nodeId").and_then(|n| n.as_u64()).unwrap_or(0);
            let fixture = dom_fixture();
            match fixture
                .selectors_by_id
                .get(&nid)
                .and_then(|sel| fixture.boxes.get(sel))
            {
                Some(b) => {
                    let [x1, y1, x2, y2] = *b;
                    if (x2 - x1) <= 0.0 || (y2 - y1) <= 0.0 {
                        // Real Chromium returns -32000 for zero-area /
                        // hidden / detached elements. The hit_test helper
                        // maps any DOM.getBoxModel error to
                        // ShimFailureKind::HitTestFailed.
                        return json!({
                            "__cdp_error__": {
                                "code": -32000,
                                "message": "Could not compute box model.",
                            }
                        });
                    }
                    let w = (x2 - x1) as u64;
                    let h = (y2 - y1) as u64;
                    json!({
                        "model": {
                            "content": [x1, y1, x2, y1, x2, y2, x1, y2],
                            "padding": [x1, y1, x2, y1, x2, y2, x1, y2],
                            "border":  [x1, y1, x2, y1, x2, y2, x1, y2],
                            "margin":  [x1, y1, x2, y1, x2, y2, x1, y2],
                            "width":  w,
                            "height": h,
                        }
                    })
                }
                None => json!({
                    "__cdp_error__": {
                        "code": -32000,
                        "message": "Could not find node with given id",
                    }
                }),
            }
        }
        "Page.getLayoutMetrics" => {
            let [w, h] = dom_fixture().viewport;
            json!({
                "layoutViewport":   { "pageX": 0, "pageY": 0, "clientWidth": w, "clientHeight": h },
                "visualViewport":   { "offsetX": 0.0, "offsetY": 0.0, "pageX": 0.0, "pageY": 0.0, "clientWidth": w, "clientHeight": h, "scale": 1.0, "zoom": 1.0 },
                "cssLayoutViewport":{ "pageX": 0, "pageY": 0, "clientWidth": w, "clientHeight": h },
                "contentSize":      { "x": 0.0, "y": 0.0, "width": w, "height": h },
                "cssContentSize":   { "x": 0.0, "y": 0.0, "width": w, "height": h },
            })
        }
        "Page.captureScreenshot" => json!({
            // 1×1 transparent PNG, base64-encoded.
            "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII="
        }),
        "Page.addScriptToEvaluateOnNewDocument" => json!({
            "identifier": "1"
        }),
        "Target.createTarget" => json!({
            "targetId": "fake-target-1"
        }),
        "Target.attachToTarget" => json!({
            // Stable id so integration tests can assert echo-back.
            "sessionId": "fake-session-1"
        }),
        "Page.enable" => json!({}),
        "Network.enable" => json!({}),
        "DOM.enable" => json!({}),
        "Runtime.enable" => json!({}),
        // Fetch domain methods used by
        // the network_interceptor's blocklist gate. All return empty
        // success per CDP convention.
        "Fetch.enable" => json!({}),
        "Fetch.continueRequest" => json!({}),
        "Fetch.failRequest" => json!({}),
        "Target.getTargets" => json!({
            "targetInfos": []
        }),
        // Page.enable, Network.enable, DOM.enable, etc.
        _ => json!({}),
    }
}

/// settle-capture: deterministic per-tick script for the readiness probe,
/// loaded once from `LOOM_FAKE_CHROMIUM_SCRIPT`. Absent env → a single
/// "immediately settled" entry, reproducing the legacy hard-coded probe
/// response (`[true,"https://fake.test/",0]`) so every non-settle test is
/// unaffected.
#[derive(Debug, Clone)]
struct SettleScript {
    /// Per-tick probe responses `(ready_complete, href, dom_mutations)`.
    /// Never empty. The last entry repeats once the cursor runs off the end.
    probe: Vec<(bool, String, u32)>,
    /// Number of never-finishing in-flight requests to pin (the never-settles
    /// network shape). Zero for normal pages.
    perpetual_inflight: usize,
}

impl SettleScript {
    /// Build the `Runtime.evaluate` response for the settle probe at tick
    /// `idx`. `result.value` is the JSON string `[ready, "href", mutations]`
    /// the host's `parse_probe` expects.
    fn probe_response(&self, idx: usize) -> Value {
        let (ready, href, muts) = self
            .probe
            .get(idx)
            .or_else(|| self.probe.last())
            .cloned()
            .unwrap_or_else(|| (true, "https://fake.test/".to_string(), 0));
        let encoded = json!([ready, href, muts]).to_string();
        json!({ "result": { "type": "string", "value": encoded } })
    }
}

fn default_settle_script() -> SettleScript {
    SettleScript {
        probe: vec![(true, "https://fake.test/".to_string(), 0)],
        perpetual_inflight: 0,
    }
}

static SETTLE_SCRIPT: OnceLock<SettleScript> = OnceLock::new();

fn settle_script() -> &'static SettleScript {
    SETTLE_SCRIPT.get_or_init(load_settle_script)
}

fn load_settle_script() -> SettleScript {
    let default = default_settle_script();
    let path = match std::env::var("LOOM_FAKE_CHROMIUM_SCRIPT") {
        Ok(p) => p,
        Err(_) => return default,
    };
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("fake-chromium: cannot read settle script {path}: {e}");
            return default;
        }
    };
    let v: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("fake-chromium: malformed settle script JSON: {e}");
            return default;
        }
    };
    let probe: Vec<(bool, String, u32)> = v
        .get("settle_probe")
        .and_then(|p| p.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    let e = e.as_array()?;
                    let ready = e.first()?.as_bool()?;
                    let href = e.get(1)?.as_str()?.to_string();
                    let muts = e.get(2)?.as_u64()? as u32;
                    Some((ready, href, muts))
                })
                .collect::<Vec<_>>()
        })
        .filter(|v| !v.is_empty())
        .unwrap_or(default.probe);
    let perpetual_inflight = v
        .get("perpetual_inflight")
        .and_then(|n| n.as_u64())
        .unwrap_or(0) as usize;
    SettleScript {
        probe,
        perpetual_inflight,
    }
}

/// Tiny DOM model used by the hit-test integration tests. Read once from
/// `LOOM_FAKE_CHROMIUM_FIXTURE` (a path to a JSON file). Empty when
/// unset.
#[derive(Debug, Clone, Default)]
struct DomFixture {
    /// Selector → bounding-box `[x1, y1, x2, y2]` in CSS pixels
    /// (top-left + bottom-right, axis-aligned).
    boxes: HashMap<String, [f64; 4]>,
    /// Selector → assigned synthetic `nodeId`.
    ids_by_selector: HashMap<String, u64>,
    /// Reverse: synthetic `nodeId` → selector.
    selectors_by_id: HashMap<u64, String>,
    /// Viewport `[width, height]` in CSS pixels. Defaults to `[1024, 768]`.
    viewport: [u64; 2],
}

static FIXTURE: OnceLock<DomFixture> = OnceLock::new();

fn dom_fixture() -> &'static DomFixture {
    FIXTURE.get_or_init(load_dom_fixture)
}

fn load_dom_fixture() -> DomFixture {
    let mut out = DomFixture {
        viewport: [1024, 768],
        ..Default::default()
    };
    let path = match std::env::var("LOOM_FAKE_CHROMIUM_FIXTURE") {
        Ok(p) => p,
        Err(_) => return out,
    };
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("fake-chromium: cannot read fixture {path}: {e}");
            return out;
        }
    };
    let v: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("fake-chromium: malformed fixture JSON: {e}");
            return out;
        }
    };
    if let Some(vp) = v.get("viewport").and_then(|x| x.as_array()) {
        if vp.len() == 2 {
            if let (Some(w), Some(h)) = (vp[0].as_u64(), vp[1].as_u64()) {
                out.viewport = [w, h];
            }
        }
    }
    if let Some(boxes_obj) = v.get("boxes").and_then(|b| b.as_object()) {
        // Stable id assignment: deterministic ordering by selector string.
        let mut keys: Vec<&String> = boxes_obj.keys().collect();
        keys.sort();
        for (i, sel) in keys.iter().enumerate() {
            let arr = match boxes_obj.get(*sel).and_then(|x| x.as_array()) {
                Some(a) if a.len() == 4 => a,
                _ => continue,
            };
            let coords = match (
                arr[0].as_f64(),
                arr[1].as_f64(),
                arr[2].as_f64(),
                arr[3].as_f64(),
            ) {
                (Some(x1), Some(y1), Some(x2), Some(y2)) => [x1, y1, x2, y2],
                _ => continue,
            };
            // Synthetic ids start at 1000 to avoid colliding with the
            // root document nodeId (1).
            let node_id = 1000_u64 + (i as u64);
            out.boxes.insert((*sel).clone(), coords);
            out.ids_by_selector.insert((*sel).clone(), node_id);
            out.selectors_by_id.insert(node_id, (*sel).clone());
        }
    }
    out
}
