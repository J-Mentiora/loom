//! Wire-level regression pins for fake-chromium's Fetch-pause fidelity.
//!
//! With `Fetch.enable {requestStage:"Request"}`, real Chromium pauses the
//! matched DOCUMENT request and the navigation does NOT proceed — no
//! `Network.responseReceived`, no `Page.loadEventFired` — until the client
//! answers `Fetch.continueRequest` / `Fetch.failRequest` for the paused
//! requestId. The fake used to emit those events fire-and-forget right
//! after the `Fetch.requestPaused` pair, so a broken interceptor
//! continue-path (wrong requestId, dropped task, never-issued
//! continueRequest) would pass the blocklist e2e against the fake while
//! hanging every real-browser navigate. These tests speak raw WS to the
//! fake binary and pin the blocking behaviour itself:
//!
//!   (a) continue: events are HELD until `Fetch.continueRequest` for the
//!       document requestId, then flush in real order (responseReceived →
//!       loadEventFired).
//!   (b) fail: `Fetch.failRequest` aborts the navigation — a Document
//!       `Network.loadingFailed` (ERR_BLOCKED_BY_CLIENT) and NO load event.
//!
//! Gated by `fake-chromium-bin` (the binary must exist). Run with:
//!   cargo build -p loom-shims --features fake-chromium-bin --bin fake-chromium
//!   cargo test -p loom-shims --features fake-chromium-bin --test fake_chromium_pause_semantics

#![cfg(feature = "fake-chromium-bin")]

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::io::BufRead;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

fn target_bin_dir() -> PathBuf {
    let test_exe = std::env::current_exe().expect("current_exe");
    let deps = test_exe.parent().expect("deps dir");
    deps.parent().expect("profile dir").to_path_buf()
}

/// Child guard: kill the fake on drop so a failing test doesn't leak it.
struct FakeChromium {
    child: Child,
    ws_url: String,
}

impl Drop for FakeChromium {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawn the fake-chromium binary and parse its
/// `DevTools listening on ws://...` startup line from stderr.
fn spawn_fake_chromium() -> FakeChromium {
    let bin = target_bin_dir().join("fake-chromium");
    assert!(
        bin.exists(),
        "fake-chromium binary not built at {}; run \
         `cargo build -p loom-shims --features fake-chromium-bin --bin fake-chromium` first",
        bin.display()
    );
    let mut child = Command::new(&bin)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn fake-chromium");
    let stderr = child.stderr.take().expect("piped stderr");
    let mut lines = std::io::BufReader::new(stderr).lines();
    let line = lines
        .next()
        .expect("fake-chromium printed nothing on stderr")
        .expect("read stderr line");
    let ws_url = line
        .strip_prefix("DevTools listening on ")
        .unwrap_or_else(|| panic!("unexpected startup line: {line}"))
        .to_string();
    FakeChromium { child, ws_url }
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(fake: &FakeChromium) -> WsStream {
    let (ws, _) = tokio_tungstenite::connect_async(&fake.ws_url)
        .await
        .expect("ws connect to fake-chromium");
    ws
}

/// Send one CDP request frame.
async fn send_cmd(ws: &mut WsStream, id: u64, method: &str, params: Value) {
    let frame = json!({"id": id, "method": method, "params": params}).to_string();
    ws.send(Message::Text(frame.into())).await.expect("ws send");
}

/// Read frames until `deadline`, returning every parsed JSON frame seen.
/// Stops early when `until` matches a frame.
async fn collect_frames(
    ws: &mut WsStream,
    window: Duration,
    until: impl Fn(&Value) -> bool,
) -> Vec<Value> {
    let mut frames = Vec::new();
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return frames;
        }
        match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                if let Ok(v) = serde_json::from_str::<Value>(&text) {
                    let stop = until(&v);
                    frames.push(v);
                    if stop {
                        return frames;
                    }
                }
            }
            Ok(Some(Ok(_))) => continue,
            Ok(_) => return frames, // closed / error
            Err(_) => return frames,
        }
    }
}

fn methods_of(frames: &[Value]) -> Vec<String> {
    frames
        .iter()
        .filter_map(|f| f.get("method").and_then(|m| m.as_str()).map(String::from))
        .collect()
}

// ── (a) document events are held until Fetch.continueRequest ───────────────

#[tokio::test]
async fn document_events_held_until_continue_request() {
    let fake = spawn_fake_chromium();
    let mut ws = connect(&fake).await;

    // Enable the Fetch gate (real Chromium only pauses after this).
    send_cmd(&mut ws, 1, "Fetch.enable", json!({})).await;
    let frames = collect_frames(&mut ws, Duration::from_secs(5), |f| {
        f.get("id").and_then(|i| i.as_u64()) == Some(1)
    })
    .await;
    assert!(!frames.is_empty(), "Fetch.enable must be acked");

    // Navigate to the tracker page: the fake pauses the document.
    send_cmd(
        &mut ws,
        2,
        "Page.navigate",
        json!({"url": "http://fake.test/page-with-tracker"}),
    )
    .await;

    // Within the observation window we must see the navigate response and
    // BOTH Fetch.requestPaused events — but the held navigation events
    // (responseReceived / loadEventFired) must NOT arrive while the
    // document pause is unanswered.
    let frames = collect_frames(&mut ws, Duration::from_millis(500), |_| false).await;
    let methods = methods_of(&frames);
    assert_eq!(
        methods
            .iter()
            .filter(|m| *m == "Fetch.requestPaused")
            .count(),
        2,
        "both Fetch.requestPaused events must be delivered; got {methods:?}"
    );
    assert!(
        !methods.contains(&"Network.responseReceived".to_string()),
        "document responseReceived must be HELD while the pause is unanswered; got {methods:?}"
    );
    assert!(
        !methods.contains(&"Page.loadEventFired".to_string()),
        "loadEventFired must be HELD while the pause is unanswered; got {methods:?}"
    );

    // Answer the document pause: the held events must flush in real order.
    send_cmd(
        &mut ws,
        3,
        "Fetch.continueRequest",
        json!({"requestId": "fake-fetch-doc-1"}),
    )
    .await;
    let frames = collect_frames(&mut ws, Duration::from_secs(5), |f| {
        f.get("method").and_then(|m| m.as_str()) == Some("Page.loadEventFired")
    })
    .await;
    let methods = methods_of(&frames);
    let resp_idx = methods
        .iter()
        .position(|m| m == "Network.responseReceived")
        .expect("responseReceived must flush after continueRequest");
    let load_idx = methods
        .iter()
        .position(|m| m == "Page.loadEventFired")
        .expect("loadEventFired must flush after continueRequest");
    assert!(
        resp_idx < load_idx,
        "flush order must match real Chromium (responseReceived before load); got {methods:?}"
    );
}

// ── (b) Fetch.failRequest aborts the held navigation ───────────────────────

#[tokio::test]
async fn document_fail_request_aborts_navigation_without_load_event() {
    let fake = spawn_fake_chromium();
    let mut ws = connect(&fake).await;

    send_cmd(&mut ws, 1, "Fetch.enable", json!({})).await;
    let _ = collect_frames(&mut ws, Duration::from_secs(5), |f| {
        f.get("id").and_then(|i| i.as_u64()) == Some(1)
    })
    .await;

    send_cmd(
        &mut ws,
        2,
        "Page.navigate",
        json!({"url": "http://fake.test/page-with-tracker"}),
    )
    .await;
    // Drain until both pauses arrived (navigate response + 2 pauses).
    let paused_seen = std::cell::Cell::new(0usize);
    let _ = collect_frames(&mut ws, Duration::from_secs(5), |f| {
        if f.get("method").and_then(|m| m.as_str()) == Some("Fetch.requestPaused") {
            paused_seen.set(paused_seen.get() + 1);
        }
        paused_seen.get() == 2
    })
    .await;
    assert_eq!(
        paused_seen.get(),
        2,
        "both pauses must arrive before answering"
    );

    // Blocking the DOCUMENT aborts the navigation.
    send_cmd(
        &mut ws,
        3,
        "Fetch.failRequest",
        json!({"requestId": "fake-fetch-doc-1", "errorReason": "BlockedByClient"}),
    )
    .await;
    let frames = collect_frames(&mut ws, Duration::from_secs(5), |f| {
        f.get("method").and_then(|m| m.as_str()) == Some("Network.loadingFailed")
    })
    .await;
    let fail = frames
        .iter()
        .find(|f| f.get("method").and_then(|m| m.as_str()) == Some("Network.loadingFailed"))
        .expect("failRequest on the document must emit Network.loadingFailed");
    assert_eq!(
        fail.pointer("/params/errorText").and_then(|v| v.as_str()),
        Some("net::ERR_BLOCKED_BY_CLIENT"),
    );

    // And the load event must never fire for an aborted navigation.
    let frames = collect_frames(&mut ws, Duration::from_millis(400), |f| {
        f.get("method").and_then(|m| m.as_str()) == Some("Page.loadEventFired")
    })
    .await;
    assert!(
        !methods_of(&frames).contains(&"Page.loadEventFired".to_string()),
        "no loadEventFired after the document was failed; got {:?}",
        methods_of(&frames)
    );
}
