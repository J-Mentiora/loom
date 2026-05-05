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

        // Runtime.evaluate is driven by an expression-pattern convention
        // (parallels Page.navigate's URL-pattern scheme above) so
        // integration tests can drive every evaluate-result branch
        // synthetically. See `parse_fake_evaluate_pattern` for the
        // sentinel grammar.
        let evaluate_response = if method == "Runtime.evaluate" {
            params
                .get("expression")
                .and_then(|v| v.as_str())
                .map(build_fake_evaluate_response)
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
fn canned_response(method: &str, params: &Value) -> Value {
    match method {
        "Page.navigate" => json!({
            "frameId": "fake-frame-1",
            "loaderId": "fake-loader-1"
        }),
        "DOM.getDocument" => json!({
            "root": {
                "nodeId": 1,
                "backendNodeId": 1,
                "nodeName": "#document",
                "nodeType": 9,
                "childNodeCount": 0,
                "children": []
            }
        }),
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
