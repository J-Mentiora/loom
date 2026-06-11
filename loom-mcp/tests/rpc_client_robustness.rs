// MCP-client robustness against daemon-down / daemon-wedged conditions.
//
// Regression coverage for two audit findings (2026-06-10):
// 1. Tool cache was primed exactly once at startup — a daemon that was down
//    at launch left `tools/list` empty forever, even after a reconnect.
//    Fixed by the `install_tool_cache_reprime` on-connected hook plus a lazy
//    re-prime in `tools_list`.
// 2. No client-side deadline on RPC responses — a wedged (alive but
//    unresponsive) daemon hung the in-flight call, the keepalive queued
//    behind the per-connection stream mutex, and therefore the whole MCP
//    server, forever. Fixed by `RpcClientConfig::call_timeout` + connection
//    poisoning in the framed caller.
//
// Fixture: a fake daemon on a temp Unix socket speaking the real framed
// protocol (4-byte BE length prefix). The client pipelines a
// `daemon.hello` ack probe after `HELLO <token>` and waits for its
// id-correlated reply — `serve_responsive` answers it like any request
// (so connect() resolves in one round trip, no 5 s stall), and
// `serve_wedged` acks the handshake then goes silent (a daemon that
// wedges after a healthy handshake).
//
// Run: cargo test -p loom-mcp --test rpc_client_robustness

use futures::{SinkExt, StreamExt};
use loom_mcp::error::LoomErrorCode;
use loom_mcp::mcp_dispatcher::McpDispatcher;
use loom_mcp::mcp_main::{build_dispatcher, install_tool_cache_reprime};
use loom_mcp::mcp_observability::McpObservability;
use loom_mcp::resource_tracker::ResourceTracker;
use loom_mcp::rpc_client::{ConnectionState, RpcClient, RpcClientConfig};
use loom_mcp::tool_cache::ToolCache;
use loom_rpc::frame_handler::FrameHandler;
use std::path::Path;
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};

// ---------------------------------------------------------------------------
// Fake daemon
// ---------------------------------------------------------------------------

/// Serve one connection: accept the HELLO, then answer every JSON-RPC
/// request — including the client's pipelined `daemon.hello` ack probe,
/// which gets a `{}` result envelope (the authenticated old-daemon
/// shape). `rpc.schemas` gets a one-method registry.
async fn serve_responsive(stream: UnixStream) {
    let mut framed = FrameHandler::wrap_stream(stream);
    let Some(Ok(_hello)) = framed.next().await else {
        return;
    };
    while let Some(Ok(frame)) = framed.next().await {
        let req: serde_json::Value = match serde_json::from_slice(&frame) {
            Ok(v) => v,
            Err(_) => return,
        };
        let result = match req.get("method").and_then(|m| m.as_str()) {
            Some("rpc.schemas") => serde_json::json!({
                "methods": [{
                    "method": "web.navigate",
                    "request": {
                        "type": "object",
                        "properties": {
                            "session": { "type": "string" },
                            "url": { "type": "string" }
                        },
                        "required": ["session", "url"]
                    },
                    "response": { "type": "object" }
                }],
                "source_wit_sha256": null
            }),
            _ => serde_json::json!({}),
        };
        let resp = serde_json::json!({
            "jsonrpc": "2.0",
            "id": req.get("id"),
            "result": result
        });
        if framed
            .send(bytes::Bytes::from(serde_json::to_vec(&resp).unwrap()))
            .await
            .is_err()
        {
            return;
        }
    }
}

/// Serve one connection wedged AFTER the handshake: accept the HELLO,
/// ack the pipelined `daemon.hello` probe (so `connect()` succeeds),
/// then read frames forever without ever responding — a daemon that is
/// alive (socket open, reads progressing) but never produces a response.
async fn serve_wedged(stream: UnixStream) {
    let mut framed = FrameHandler::wrap_stream(stream);
    let Some(Ok(_hello)) = framed.next().await else {
        return;
    };
    let Some(Ok(probe)) = framed.next().await else {
        return;
    };
    let req: serde_json::Value = serde_json::from_slice(&probe).unwrap_or_default();
    let resp = serde_json::json!({
        "jsonrpc": "2.0",
        "id": req.get("id"),
        "result": { "hello": "ok", "server": "fixture" }
    });
    let _ = framed
        .send(bytes::Bytes::from(serde_json::to_vec(&resp).unwrap()))
        .await;
    while let Some(Ok(_frame)) = framed.next().await {}
}

/// Serve one connection silent from the very first frame: reads forever,
/// never answers anything — not even the handshake probe. A daemon
/// wedged BEFORE the handshake completes.
async fn serve_silent(stream: UnixStream) {
    let mut framed = FrameHandler::wrap_stream(stream);
    while let Some(Ok(_frame)) = framed.next().await {}
}

/// Config pointed at a temp socket + token, with a test-scale call deadline.
fn fixture_config(dir: &Path, call_timeout: Duration) -> RpcClientConfig {
    let token_path = dir.join("hello.token");
    std::fs::write(&token_path, "fixture-token\n").unwrap();
    let mut cfg = RpcClientConfig::defaults();
    cfg.socket_path = dir.join("loomd.sock");
    cfg.hello_token_path = token_path;
    cfg.call_timeout = call_timeout;
    cfg
}

// ---------------------------------------------------------------------------
// Finding 1 — tool cache re-primes on reconnect
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reconnect_reprimes_tool_cache_after_daemon_down_at_launch() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = fixture_config(dir.path(), Duration::from_secs(2));
    let socket_path = cfg.socket_path.clone();
    let obs = McpObservability::new(true);
    let rpc = RpcClient::new(cfg, obs.clone());
    // Hold the ToolCache externally so the test can poll it directly —
    // `dispatcher.tools_list()` would itself lazily re-prime and mask the
    // on-connected hook under test.
    let tool_cache = ToolCache::new(rpc.clone());
    let dispatcher = McpDispatcher::new(
        tool_cache.clone(),
        ResourceTracker::new(rpc.clone()),
        rpc.clone(),
        obs.clone(),
        tokio_util::sync::CancellationToken::new(),
        loom_mcp::mcp_dispatcher::SessionOptions::default(),
    );
    install_tool_cache_reprime(&rpc, dispatcher.clone(), obs.clone()).await;

    // Daemon down at launch: connect and the startup prime both fail and the
    // cache stays empty — mirrors mcp_main::run's graceful degradation.
    assert!(
        rpc.connect().await.is_err(),
        "no listener yet: connect must fail"
    );
    assert!(dispatcher.prime_tool_cache().await.is_err());
    assert!(tool_cache.list().await.is_empty());

    // Daemon comes up.
    let listener = UnixListener::bind(&socket_path).unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(serve_responsive(stream));
        }
    });

    // Every production reconnect path (keepalive ping, call-path
    // reconnect-first, backoff task) converges on connect(); drive it
    // directly. Success must fire the re-prime hook (one ack round
    // trip — no HELLO stall).
    rpc.connect().await.expect("reconnect must succeed");

    // The hook primes on a spawned task; poll until it lands.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let tools = tool_cache.list().await;
        if !tools.is_empty() {
            assert!(
                tools.iter().any(|t| t.name == "loom.web.navigate"),
                "re-primed cache must carry the daemon's method set; got {tools:?}"
            );
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "tool cache was not re-primed within 10 s of a successful reconnect"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn tools_list_lazily_primes_once_daemon_is_reachable() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = fixture_config(dir.path(), Duration::from_secs(2));
    let socket_path = cfg.socket_path.clone();
    let obs = McpObservability::new(true);
    let rpc = RpcClient::new(cfg, obs.clone());
    // build_dispatcher only — deliberately NO re-prime hook, to isolate the
    // lazy tools/list recovery path.
    let dispatcher = build_dispatcher(
        rpc.clone(),
        obs.clone(),
        tokio_util::sync::CancellationToken::new(),
        loom_mcp::mcp_dispatcher::SessionOptions::default(),
    )
    .await;

    // Daemon down: the lazy prime fails fast and the daemon-derived list
    // degrades to nothing — only the MCP-server-local loom.session.* tools
    // (schemas defined in-process, no daemon round trip) are advertised.
    assert!(dispatcher
        .tools_list()
        .await
        .iter()
        .all(|t| t.name.starts_with("loom.session.")));

    // Daemon comes up.
    let listener = UnixListener::bind(&socket_path).unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(serve_responsive(stream));
        }
    });

    // The same tools/list call now recovers on its own: the lazy prime
    // reconnects (reconnect-first inside RpcClient::call, one ack round
    // trip) and serves the real method set instead of [] until restart.
    let tools = dispatcher.tools_list().await;
    assert!(
        tools.iter().any(|t| t.name == "loom.web.navigate"),
        "lazy re-prime must populate tools/list once the daemon is reachable; got {tools:?}"
    );
}

// ---------------------------------------------------------------------------
// Finding 2 — client-side deadline on RPC responses
// ---------------------------------------------------------------------------

#[tokio::test]
async fn non_responding_daemon_times_out_call_instead_of_hanging() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = fixture_config(dir.path(), Duration::from_millis(300));
    let socket_path = cfg.socket_path.clone();
    let listener = UnixListener::bind(&socket_path).unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(serve_wedged(stream));
        }
    });
    let obs = McpObservability::new(true);
    let rpc = RpcClient::new(cfg, obs);
    rpc.connect().await.expect("handshake must be accepted");

    // web.click is non-idempotent: a possibly-dispatched timeout must NOT
    // auto-resend, so the typed error surfaces directly. Pre-fix this await
    // never returned; the 10 s wrapper is the regression tripwire.
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        rpc.call(
            "web.click",
            serde_json::json!({ "session": "s", "selector": "#a" }),
        ),
    )
    .await
    .expect("call must observe the client-side deadline instead of hanging");

    let err = result.expect_err("wedged daemon must yield a typed error");
    assert_eq!(
        err.code,
        LoomErrorCode::TransportDropped,
        "deadline expiry must surface as transport_dropped so the reconnect \
         policy engages; got {err:?}"
    );
    assert_eq!(
        err.context
            .as_ref()
            .and_then(|c| c.get("dispatch_phase"))
            .and_then(|v| v.as_str()),
        Some("post"),
        "the request was sent → possibly dispatched → POST phase"
    );
    // The connection was marked dead so the reconnect path takes over. (The
    // background reconnect task may already be in Connecting; the invariant
    // is that the wedged connection is never reported healthy.)
    assert_ne!(rpc.state().await, ConnectionState::Connected);
}

#[tokio::test]
async fn keepalive_ping_survives_a_wedged_call() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = fixture_config(dir.path(), Duration::from_millis(300));
    let socket_path = cfg.socket_path.clone();
    let listener = UnixListener::bind(&socket_path).unwrap();
    // First connection wedges (alive but never answers); every later
    // connection is served normally — a daemon stall that has resolved by
    // the time the client reconnects.
    tokio::spawn(async move {
        let mut accepted = 0usize;
        while let Ok((stream, _)) = listener.accept().await {
            accepted += 1;
            if accepted == 1 {
                tokio::spawn(serve_wedged(stream));
            } else {
                tokio::spawn(serve_responsive(stream));
            }
        }
    });
    let obs = McpObservability::new(true);
    let rpc = RpcClient::new(cfg, obs);
    rpc.connect().await.expect("handshake must be accepted");

    // Wedge the connection with a call that will never be answered.
    let wedged = {
        let rpc = rpc.clone();
        tokio::spawn(async move {
            rpc.call("web.click", serde_json::json!({ "session": "s" }))
                .await
        })
    };
    // Let the wedged call acquire the per-connection stream and send.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Pre-fix this ping blocked forever on the stream mutex held by the
    // wedged call (the keepalive deadlock from the audit). Post-fix: the
    // wedged call times out (300 ms) and poisons the connection, the ping
    // fails fast on the poisoned stream, reconnects (health.ping is
    // idempotent) and succeeds on the fresh connection. The 30 s budget
    // is generous headroom for the reconnect round trips.
    tokio::time::timeout(Duration::from_secs(30), rpc.ping())
        .await
        .expect("keepalive must not deadlock behind a wedged call")
        .expect("ping must succeed after the automatic reconnect");

    let wedged_err = wedged
        .await
        .unwrap()
        .expect_err("the wedged call itself must fail with the typed deadline error");
    assert_eq!(wedged_err.code, LoomErrorCode::TransportDropped);
}

// ---------------------------------------------------------------------------
// Connection-protocol redesign — HELLO ack handshake
// ---------------------------------------------------------------------------

/// A daemon wedged BEFORE answering the handshake probe fails connect()
/// with a typed transport error in bounded time — previously 5 s of
/// silence was misread as 'HELLO accepted' and the wedge surfaced only
/// on the first call.
#[tokio::test]
async fn wedged_handshake_fails_connect_with_typed_transport_error() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = fixture_config(dir.path(), Duration::from_millis(300));
    let socket_path = cfg.socket_path.clone();
    let listener = UnixListener::bind(&socket_path).unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(serve_silent(stream));
        }
    });
    let obs = McpObservability::new(true);
    let rpc = RpcClient::new(cfg, obs);

    let err = tokio::time::timeout(Duration::from_secs(10), rpc.connect())
        .await
        .expect("connect must observe its handshake bound")
        .expect_err("a silent daemon must fail the handshake");
    assert_eq!(err.code, LoomErrorCode::TransportDropped);
}

/// Old-daemon compat: a pre-ack daemon (≤0.10.x) answers the probe with
/// a `method_not_found` envelope — connect() must treat that as
/// authenticated and proceed to serve calls normally.
#[tokio::test]
async fn old_daemon_method_not_found_probe_reply_counts_as_authenticated() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = fixture_config(dir.path(), Duration::from_secs(2));
    let socket_path = cfg.socket_path.clone();
    let listener = UnixListener::bind(&socket_path).unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut framed = FrameHandler::wrap_stream(stream);
                let Some(Ok(_hello)) = framed.next().await else {
                    return;
                };
                // Old daemons don't know daemon.hello: answer the probe
                // with the validator's method_not_found envelope, then
                // serve later requests normally.
                while let Some(Ok(frame)) = framed.next().await {
                    let req: serde_json::Value = serde_json::from_slice(&frame).unwrap_or_default();
                    let resp = match req.get("method").and_then(|m| m.as_str()) {
                        Some("daemon.hello") => serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": req.get("id"),
                            "error": {
                                "code": "method_not_found",
                                "message": "method not found: daemon.hello"
                            }
                        }),
                        _ => serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": req.get("id"),
                            "result": {}
                        }),
                    };
                    if framed
                        .send(bytes::Bytes::from(serde_json::to_vec(&resp).unwrap()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
    });
    let obs = McpObservability::new(true);
    let rpc = RpcClient::new(cfg, obs);

    rpc.connect()
        .await
        .expect("method_not_found probe reply must count as authenticated");
    rpc.ping()
        .await
        .expect("calls must work on the old-daemon connection");
}

/// Auth rejection: the daemon's bare typed JsonRpcError (no envelope
/// keys) followed by close must surface as the typed HELLO mismatch,
/// never as success.
#[tokio::test]
async fn bare_error_frame_surfaces_as_hello_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = fixture_config(dir.path(), Duration::from_secs(2));
    let socket_path = cfg.socket_path.clone();
    let listener = UnixListener::bind(&socket_path).unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut framed = FrameHandler::wrap_stream(stream);
                let Some(Ok(_hello)) = framed.next().await else {
                    return;
                };
                let bare = serde_json::json!({
                    "code": "protocol_auth_required",
                    "message": "authentication required"
                });
                let _ = framed
                    .send(bytes::Bytes::from(serde_json::to_vec(&bare).unwrap()))
                    .await;
                // Close, like the real daemon.
            });
        }
    });
    let obs = McpObservability::new(true);
    let rpc = RpcClient::new(cfg, obs);

    let err = rpc
        .connect()
        .await
        .expect_err("rejection must fail connect");
    assert_eq!(err.code, LoomErrorCode::RpcAuthFailed);
}
