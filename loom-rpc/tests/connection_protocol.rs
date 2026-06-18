//! Wire-level regression tests for the connection-protocol redesign:
//! concurrent per-connection dispatch, the `request.cancel` fast lane,
//! per-action `deadline_ms` clamping, and the opt-in HELLO ack.
//!
//! These tests drive a real `SocketServer` over a temp Unix socket with
//! a purpose-built `RequestRouterApi` (slow async / blocking-bridge /
//! echo methods) so request-lifecycle behavior is observable without
//! the full daemon stack.

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use loom_rpc::{
    auth_middleware::auth_middleware::{AuthMiddleware, Token},
    connection_handler::connection_handler::ConnectionHandlerDeps,
    frame_handler::frame_handler::FrameHandler,
    request_router::request_router::RequestRouterApi,
    rpc_observability::rpc_observability::RpcObservability,
    schema_validator::schema_validator::SchemaValidator,
    socket_server::socket_server::{SocketServer, SocketServerConfig},
};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::net::UnixStream;

mod common;

type Framed = tokio_util::codec::Framed<UnixStream, tokio_util::codec::LengthDelimitedCodec>;

// ─── Test router ─────────────────────────────────────────────────────────────

/// Router with controllable latency per method:
/// - `test.echo`  — answers immediately with `{"echo": true}`.
/// - `test.slow`  — async-sleeps `params.sleep_ms` (default 1000) then
///   answers `{"done": true}`. Cancellable at the next await point.
struct TestRouter;

#[async_trait::async_trait]
impl RequestRouterApi for TestRouter {
    async fn dispatch(&self, method: &str, params: serde_json::Value) -> Vec<u8> {
        match method {
            "test.echo" => serde_json::to_vec(&serde_json::json!({"echo": true})).unwrap(),
            "test.slow" => {
                let ms = params
                    .get("sleep_ms")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1000);
                tokio::time::sleep(Duration::from_millis(ms)).await;
                serde_json::to_vec(&serde_json::json!({"done": true})).unwrap()
            }
            other => serde_json::to_vec(&serde_json::json!({
                "__loom_rpc_error": true,
                "code": "method_not_found",
                "message": format!("method not found: {other}"),
            }))
            .unwrap(),
        }
    }

    fn methods(&self) -> Vec<String> {
        vec!["test.echo".into(), "test.slow".into()]
    }
}

// ─── Harness ─────────────────────────────────────────────────────────────────

struct TestServer {
    _dir: TempDir,
    token: Token,
    socket_path: std::path::PathBuf,
}

async fn start_server() -> (TestServer, tokio::task::JoinHandle<()>) {
    start_server_with_router(Arc::new(TestRouter)).await
}

async fn start_server_with_router(
    router: Arc<dyn RequestRouterApi>,
) -> (TestServer, tokio::task::JoinHandle<()>) {
    // Hold the bind lock across the WHOLE synchronous setup — TempDir creation AND the bind.
    // `try_bind`'s process-global umask would otherwise corrupt a concurrent test's
    // `TempDir::new()` (see common::BIND_LOCK). The guard is a sync mutex dropped at the end
    // of this block, BEFORE the first `.await` below, so the future stays Send.
    let (dir, socket_path, token, server) = {
        let _bind = common::bind_guard();
        let dir = TempDir::new().unwrap();
        let socket_path = dir.path().join("test.sock");

        let token = Token::generate();
        let token_arc = Arc::new(token.clone());
        let provider: Arc<dyn loom_rpc::schema_provider::schema_provider::SchemaProviderApi> =
            Arc::new(common::EmptySchemaProvider);

        let deps = Arc::new(ConnectionHandlerDeps {
            auth: AuthMiddleware::new(token_arc),
            validator: SchemaValidator::new(provider),
            router,
            observability: RpcObservability::new(),
        });
        let cfg = SocketServerConfig {
            socket_path: socket_path.clone(),
            token_override: Some(token.clone()),
        };
        let server = SocketServer::new(cfg, deps).expect("SocketServer::new must succeed");
        (dir, socket_path, token, server)
    };
    let handle = tokio::runtime::Handle::current();
    let join = tokio::spawn(async move {
        server.serve(handle, futures::future::pending::<()>()).await;
    });
    // Allow the accept loop to start.
    tokio::time::sleep(Duration::from_millis(50)).await;

    (
        TestServer {
            _dir: dir,
            token,
            socket_path,
        },
        join,
    )
}

async fn connect_and_hello(srv: &TestServer) -> Framed {
    let stream = UnixStream::connect(&srv.socket_path)
        .await
        .expect("connect must succeed");
    let mut framed = FrameHandler::wrap_stream(stream);
    framed
        .send(Bytes::from(format!("HELLO {}", srv.token.0).into_bytes()))
        .await
        .unwrap();
    framed
}

async fn send_json(framed: &mut Framed, v: &serde_json::Value) {
    framed
        .send(Bytes::from(serde_json::to_vec(v).unwrap()))
        .await
        .unwrap();
}

async fn recv_json(framed: &mut Framed) -> serde_json::Value {
    let bytes = framed
        .next()
        .await
        .expect("frame expected")
        .expect("no codec error");
    serde_json::from_slice(&bytes).expect("response must be valid JSON")
}

fn request(id: serde_json::Value, method: &str, params: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn error_code(resp: &serde_json::Value) -> &str {
    resp.get("error")
        .and_then(|e| e.get("code"))
        .and_then(|c| c.as_str())
        .unwrap_or("")
}

// ─── Concurrent dispatch + out-of-order correlation ──────────────────────────

/// A fast request pipelined behind a slow one must answer FIRST; both
/// responses correlate by JSON-RPC id (the connection no longer
/// processes frames serially).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipelined_out_of_order_responses_correlate_by_id() {
    let (srv, _bg) = start_server().await;
    let mut framed = connect_and_hello(&srv).await;

    send_json(
        &mut framed,
        &request(1.into(), "test.slow", serde_json::json!({"sleep_ms": 800})),
    )
    .await;
    send_json(
        &mut framed,
        &request(2.into(), "test.echo", serde_json::json!({})),
    )
    .await;

    let first = recv_json(&mut framed).await;
    assert_eq!(
        first.get("id").and_then(|v| v.as_i64()),
        Some(2),
        "fast request must answer before the slow one; got: {first}"
    );
    assert_eq!(first["result"]["echo"], serde_json::json!(true));

    let second = recv_json(&mut framed).await;
    assert_eq!(second.get("id").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(second["result"]["done"], serde_json::json!(true));
}

// ─── request.cancel actually cancels ─────────────────────────────────────────

/// `request.cancel` must (a) answer on its fast lane while the target is
/// still in flight, with `cancelled: true`, and (b) actually abort the
/// in-flight dispatch — the target answers `request_cancelled` well
/// before its sleep would have completed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn request_cancel_cancels_a_slow_in_flight_request() {
    let (srv, _bg) = start_server().await;
    let mut framed = connect_and_hello(&srv).await;
    let started = Instant::now();

    send_json(
        &mut framed,
        &request(1.into(), "test.slow", serde_json::json!({"sleep_ms": 5000})),
    )
    .await;
    // Give the dispatch task time to start before cancelling.
    tokio::time::sleep(Duration::from_millis(150)).await;
    send_json(
        &mut framed,
        &request(
            2.into(),
            "request.cancel",
            serde_json::json!({"request_id": 1}),
        ),
    )
    .await;

    let cancel_resp = recv_json(&mut framed).await;
    assert_eq!(cancel_resp.get("id").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(
        cancel_resp["result"]["cancelled"],
        serde_json::json!(true),
        "cancel must find the in-flight token; got: {cancel_resp}"
    );

    let target_resp = recv_json(&mut framed).await;
    assert_eq!(target_resp.get("id").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(
        error_code(&target_resp),
        "request_cancelled",
        "target must abort with the typed cancel code; got: {target_resp}"
    );
    assert!(
        started.elapsed() < Duration::from_millis(3000),
        "cancelled request must not run out its full sleep; took {:?}",
        started.elapsed()
    );
}

/// Cancelling an id with nothing in flight is idempotent: `cancelled: false`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_unknown_id_returns_false() {
    let (srv, _bg) = start_server().await;
    let mut framed = connect_and_hello(&srv).await;

    send_json(
        &mut framed,
        &request(
            1.into(),
            "request.cancel",
            serde_json::json!({"request_id": 99}),
        ),
    )
    .await;
    let resp = recv_json(&mut framed).await;
    assert_eq!(resp["result"]["cancelled"], serde_json::json!(false));
}

/// A second request reusing a LIVE in-flight id is rejected with
/// `protocol_malformed` (correlation would be ambiguous); the original
/// request still completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duplicate_in_flight_id_is_rejected() {
    let (srv, _bg) = start_server().await;
    let mut framed = connect_and_hello(&srv).await;

    send_json(
        &mut framed,
        &request(7.into(), "test.slow", serde_json::json!({"sleep_ms": 800})),
    )
    .await;
    send_json(
        &mut framed,
        &request(7.into(), "test.echo", serde_json::json!({})),
    )
    .await;

    let first = recv_json(&mut framed).await;
    assert_eq!(error_code(&first), "protocol_malformed", "got: {first}");

    let second = recv_json(&mut framed).await;
    assert_eq!(second["result"]["done"], serde_json::json!(true));
}

/// Serial single-in-flight clients (CLI, python sync SDK, loom-mcp
/// FramedCaller) see exactly the old behavior: one request, one ordered
/// response — including reusing the same id call after call.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn serial_client_with_reused_id_is_unchanged() {
    let (srv, _bg) = start_server().await;
    let mut framed = connect_and_hello(&srv).await;

    for _ in 0..3 {
        send_json(
            &mut framed,
            &request(1.into(), "test.echo", serde_json::json!({})),
        )
        .await;
        let resp = recv_json(&mut framed).await;
        assert_eq!(resp.get("id").and_then(|v| v.as_i64()), Some(1));
        assert_eq!(resp["result"]["echo"], serde_json::json!(true));
    }
}

// ─── spawn_blocking: cancel/timeout preempt BLOCKING dispatch ────────────────

use loom_rpc::host_service_adapter::host_service_adapter::HostServiceAdapterApi;
use loom_rpc::host_service_adapter::host_service_adapter::{
    Action, AdapterError, HostServiceAdapter, Receipt, WasmHostBridge,
};

/// Bridge whose blocking dispatch sleeps on its (blocking) thread —
/// exactly the shape of a slow/wedged surface verb.
struct SlowBlockingBridge {
    delay: Duration,
}

impl WasmHostBridge for SlowBlockingBridge {
    fn dispatch_action_blocking(&self, _action: Action) -> Result<Receipt, AdapterError> {
        std::thread::sleep(self.delay);
        Err(loom_rpc::error_translator::error_translator::LoomErrorCode::SurfaceUnavailable)
    }
}

/// Router whose `test.blocking` goes through the REAL
/// `HostServiceAdapter::dispatch_action` (the spawn_blocking path), so
/// this test pins the production surface-verb dispatch shape.
struct BlockingRouter {
    host: Arc<HostServiceAdapter>,
}

#[async_trait::async_trait]
impl RequestRouterApi for BlockingRouter {
    async fn dispatch(&self, method: &str, _params: serde_json::Value) -> Vec<u8> {
        match method {
            "test.blocking" => {
                let action = Action::WebNavigate {
                    session_id: "s".into(),
                    url: "https://example.com".into(),
                    until: None,
                    timeout_ms: None,
                };
                match self.host.dispatch_action(action).await {
                    Ok(receipt) => serde_json::to_vec(&receipt).unwrap(),
                    Err(code) => serde_json::to_vec(&serde_json::json!({
                        "__loom_rpc_error": true,
                        "code": code,
                        "message": "action dispatch failed",
                    }))
                    .unwrap(),
                }
            }
            _ => serde_json::to_vec(&serde_json::json!({
                "__loom_rpc_error": true,
                "code": "method_not_found",
                "message": "method not found",
            }))
            .unwrap(),
        }
    }

    fn methods(&self) -> Vec<String> {
        vec!["test.blocking".into()]
    }
}

/// With the dispatch on tokio's blocking pool (spawn_blocking), the
/// per-request select! arms stay live: request.cancel preempts a
/// BLOCKING surface-verb dispatch with a typed `request_cancelled`
/// while the abandoned work runs out detached. (Under the previous
/// block_in_place design the select! could not fire until the blocking
/// call returned — this test would take the full 3 s and see
/// `surface_unavailable`.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_preempts_blocking_surface_dispatch() {
    let host = HostServiceAdapter::new(Arc::new(SlowBlockingBridge {
        delay: Duration::from_millis(3000),
    }) as Arc<dyn WasmHostBridge>);
    let (srv, _bg) = start_server_with_router(Arc::new(BlockingRouter { host })).await;
    let mut framed = connect_and_hello(&srv).await;
    let started = Instant::now();

    send_json(
        &mut framed,
        &request(1.into(), "test.blocking", serde_json::json!({})),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    send_json(
        &mut framed,
        &request(
            2.into(),
            "request.cancel",
            serde_json::json!({"request_id": 1}),
        ),
    )
    .await;

    let cancel_resp = recv_json(&mut framed).await;
    assert_eq!(cancel_resp.get("id").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(cancel_resp["result"]["cancelled"], serde_json::json!(true));

    let target_resp = recv_json(&mut framed).await;
    assert_eq!(target_resp.get("id").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(
        error_code(&target_resp),
        "request_cancelled",
        "blocking dispatch must be preempted by cancel; got: {target_resp}"
    );
    assert!(
        started.elapsed() < Duration::from_millis(2000),
        "cancel must answer while the blocking work is still running; took {:?}",
        started.elapsed()
    );
}

// ─── deadline_ms clamps the server-side request deadline ─────────────────────

/// An SDK action envelope's `deadline_ms` bounds the request: a slow
/// dispatch is killed with the typed `request_timeout` at ~the action
/// deadline, not the 30 s server default. (Previously the knob was
/// silently discarded by unwrap_sdk_envelope and every action ran to
/// the server default.)
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn action_deadline_ms_clamps_request_deadline() {
    let (srv, _bg) = start_server().await;
    let mut framed = connect_and_hello(&srv).await;
    let started = Instant::now();

    // SDK envelope shape: payload bytes carry the (flattened-on-unwrap)
    // inner params; deadline_ms rides on the action envelope.
    let payload: Vec<u8> = br#"{"sleep_ms": 10000}"#.to_vec();
    send_json(
        &mut framed,
        &request(
            1.into(),
            "test.slow",
            serde_json::json!({
                "session_id": "s",
                "action": { "kind": "slow", "payload": payload, "deadline_ms": 300 }
            }),
        ),
    )
    .await;

    let resp = recv_json(&mut framed).await;
    assert_eq!(resp.get("id").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(
        error_code(&resp),
        "request_timeout",
        "short deadline_ms must bound the slow action; got: {resp}"
    );
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("deadline_ms"),
        "timeout message must attribute the action deadline; got: {resp}"
    );
    assert!(
        started.elapsed() < Duration::from_millis(2500),
        "deadline_ms=300 must fire long before the 10 s sleep; took {:?}",
        started.elapsed()
    );
}

/// Flat-shape callers (CLI / MCP) that send no deadline keep the server
/// default: a fast request completes normally, nothing times out early.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_deadline_keeps_server_default() {
    let (srv, _bg) = start_server().await;
    let mut framed = connect_and_hello(&srv).await;

    send_json(
        &mut framed,
        &request(1.into(), "test.slow", serde_json::json!({"sleep_ms": 200})),
    )
    .await;
    let resp = recv_json(&mut framed).await;
    assert_eq!(resp["result"]["done"], serde_json::json!(true));
}

// ─── HELLO ack handshake matrix (daemon side) ────────────────────────────────

/// new-client ↔ new-daemon: a `daemon.hello` probe pipelined after the
/// HELLO frame is answered with the explicit ack
/// `{"hello":"ok","server":"<version>"}` as its id-correlated result.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hello_ack_probe_gets_explicit_ack() {
    let (srv, _bg) = start_server().await;
    let mut framed = connect_and_hello(&srv).await;

    send_json(
        &mut framed,
        &request(0.into(), "daemon.hello", serde_json::json!({})),
    )
    .await;

    let resp = recv_json(&mut framed).await;
    assert_eq!(resp.get("id").and_then(|v| v.as_i64()), Some(0));
    assert_eq!(resp["result"]["hello"], serde_json::json!("ok"));
    assert_eq!(
        resp["result"]["server"],
        serde_json::json!(env!("CARGO_PKG_VERSION")),
        "ack must disclose the server version; got: {resp}"
    );
}

/// old-client ↔ new-daemon: a client that never sends the opt-in probe
/// must never receive an unsolicited post-HELLO frame — its first
/// response is exactly the response to its first request, so the
/// deployed ≤0.10.x "any post-HELLO frame = rejection" timeout
/// heuristic keeps working.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_opt_in_means_no_ack_frame() {
    let (srv, _bg) = start_server().await;
    let mut framed = connect_and_hello(&srv).await;

    // Old-client behavior: HELLO, brief silence (its timeout window),
    // then the first real request.
    let unsolicited =
        tokio::time::timeout(Duration::from_millis(250), recv_json(&mut framed)).await;
    assert!(
        unsolicited.is_err(),
        "daemon must stay silent after HELLO unless the client opted in; got: {unsolicited:?}"
    );

    send_json(
        &mut framed,
        &request(1.into(), "test.echo", serde_json::json!({})),
    )
    .await;
    let resp = recv_json(&mut framed).await;
    assert_eq!(resp.get("id").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(resp["result"]["echo"], serde_json::json!(true));
}

/// Auth failure with a pipelined probe: the daemon sends the bare typed
/// JsonRpcError (no JSON-RPC envelope keys) and closes — the probe is
/// never answered. New clients parse the bare frame typed; close
/// without an ack is auth failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bad_hello_with_pipelined_probe_gets_bare_error_then_close() {
    let (srv, _bg) = start_server().await;
    let stream = UnixStream::connect(&srv.socket_path).await.unwrap();
    let mut framed = FrameHandler::wrap_stream(stream);
    framed
        .send(Bytes::from(&b"HELLO wrong-token"[..]))
        .await
        .unwrap();
    // Pipeline the probe onto a connection the daemon is about to reject.
    // This send races the rejection: the daemon may already have closed
    // the socket, so a `BrokenPipe` here is an accepted outcome — it only
    // means the probe never reached the daemon (still "never answered").
    // Tolerate the send error rather than `.unwrap()`-ing it.
    let _ = framed
        .send(Bytes::from(
            serde_json::to_vec(&request(0.into(), "daemon.hello", serde_json::json!({}))).unwrap(),
        ))
        .await;

    let resp = recv_json(&mut framed).await;
    assert_eq!(
        resp.get("code").and_then(|c| c.as_str()),
        Some("protocol_auth_required"),
        "rejection must be the bare typed error; got: {resp}"
    );
    assert!(
        resp.get("jsonrpc").is_none() && resp.get("id").is_none(),
        "rejection frame is bare (no envelope keys); got: {resp}"
    );
    // The connection closes without ever acking the probe. The close
    // surfaces either as a clean EOF (`None`) or — when the daemon closes
    // with the pipelined probe still unread in its receive buffer — as a
    // transport reset (`Some(Err(_))`, e.g. ECONNRESET) on this read. Both
    // mean "closed, probe never answered"; the bare error is always
    // delivered ahead of the reset, so the assertions above stay reliable.
    // The only forbidden outcome is the daemon actually answering the
    // probe with a frame.
    match framed.next().await {
        None | Some(Err(_)) => {}
        Some(Ok(frame)) => panic!(
            "daemon must close after auth rejection, never ack the probe; got a frame: {frame:?}"
        ),
    }
}

/// The probe is answered on the fast lane even while a slow dispatch
/// occupies the connection (it must never queue behind one).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hello_ack_is_not_queued_behind_slow_dispatch() {
    let (srv, _bg) = start_server().await;
    let mut framed = connect_and_hello(&srv).await;

    send_json(
        &mut framed,
        &request(1.into(), "test.slow", serde_json::json!({"sleep_ms": 2000})),
    )
    .await;
    send_json(
        &mut framed,
        &request(0.into(), "daemon.hello", serde_json::json!({})),
    )
    .await;

    let resp = recv_json(&mut framed).await;
    assert_eq!(
        resp.get("id").and_then(|v| v.as_i64()),
        Some(0),
        "ack must arrive before the slow dispatch completes; got: {resp}"
    );
    assert_eq!(resp["result"]["hello"], serde_json::json!("ok"));
}

// ─── per-session in-order serialization guard ────────────────────────────────
//
// Two pipelined SAME-session surface verbs must execute in SUBMISSION order
// (the per-session ordered worker), while DIFFERENT-session verbs on one
// connection still dispatch concurrently. Modeled on the MCP ordered-lane
// behavior; the mock router uses real `web.*` method names so the
// connection handler's `session_ordered_key` classifies them onto the FIFO
// lane (every `web.*` is in `known_router_methods()`).

use std::collections::HashSet;
use std::sync::Mutex;

/// One observed dispatch window: which session+method ran, and when it
/// entered/exited the router. Overlapping windows on different sessions
/// prove concurrency; non-overlap on one session proves serialization.
#[derive(Clone)]
struct DispatchWindow {
    session: String,
    method: String,
    enter: Instant,
    exit: Instant,
}

#[derive(Default)]
struct OrderingState {
    /// Sessions whose `web.navigate` has fully completed.
    navigated: HashSet<String>,
    /// Every dispatch's window, in completion order.
    windows: Vec<DispatchWindow>,
    /// (session, method) at dispatch ENTER, in entry order — the
    /// observable proxy for WAL/receipt append order.
    enter_log: Vec<(String, String)>,
}

/// Router with real `web.*` verbs: `web.navigate` is slow (async sleep,
/// then marks its session navigated); `web.evaluate` is fast and reports
/// whether its session had already navigated at dispatch-enter time. Both
/// record their enter order + window so a test can assert per-session
/// ordering and cross-session overlap.
struct OrderingRouter {
    state: Arc<Mutex<OrderingState>>,
    nav_delay: Duration,
}

fn session_of(params: &serde_json::Value) -> String {
    params
        .get("session")
        .or_else(|| params.get("session_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

#[async_trait::async_trait]
impl RequestRouterApi for OrderingRouter {
    async fn dispatch(&self, method: &str, params: serde_json::Value) -> Vec<u8> {
        let sid = session_of(&params);
        let enter = Instant::now();
        {
            let mut st = self.state.lock().unwrap();
            st.enter_log.push((sid.clone(), method.to_string()));
        }
        match method {
            "web.navigate" => {
                tokio::time::sleep(self.nav_delay).await;
                let exit = Instant::now();
                let mut st = self.state.lock().unwrap();
                st.navigated.insert(sid.clone());
                st.windows.push(DispatchWindow {
                    session: sid.clone(),
                    method: "web.navigate".into(),
                    enter,
                    exit,
                });
                serde_json::to_vec(&serde_json::json!({ "navigated": true, "session": sid }))
                    .unwrap()
            }
            "web.evaluate" => {
                // Snapshot whether navigate already finished for this session.
                let navigated_first = {
                    let st = self.state.lock().unwrap();
                    st.navigated.contains(&sid)
                };
                let exit = Instant::now();
                {
                    let mut st = self.state.lock().unwrap();
                    st.windows.push(DispatchWindow {
                        session: sid.clone(),
                        method: "web.evaluate".into(),
                        enter,
                        exit,
                    });
                }
                serde_json::to_vec(&serde_json::json!({
                    "navigated_first": navigated_first,
                    "session": sid,
                }))
                .unwrap()
            }
            other => serde_json::to_vec(&serde_json::json!({
                "__loom_rpc_error": true,
                "code": "method_not_found",
                "message": format!("method not found: {other}"),
            }))
            .unwrap(),
        }
    }

    fn methods(&self) -> Vec<String> {
        vec!["web.navigate".into(), "web.evaluate".into()]
    }
}

fn web_request(id: i64, method: &str, session: &str) -> serde_json::Value {
    request(
        id.into(),
        method,
        serde_json::json!({ "session": session, "url": "https://example.com", "expression": "1" }),
    )
}

/// SAME-session ordering, regression-pinned over many iterations: a
/// `web.evaluate` pipelined immediately behind a slow `web.navigate` on the
/// SAME session must (every run) observe the post-navigate state and never
/// overtake it — and never get a spurious `too_many_requests`. Without the
/// per-session FIFO worker, the later evaluate's own spawned task would race
/// ahead of the slow navigate and read pre-navigate state.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_session_pipelined_actions_execute_in_submission_order() {
    let state = Arc::new(Mutex::new(OrderingState::default()));
    let router = Arc::new(OrderingRouter {
        state: Arc::clone(&state),
        nav_delay: Duration::from_millis(60),
    });
    let (srv, _bg) = start_server_with_router(router).await;
    let mut framed = connect_and_hello(&srv).await;

    const ITERS: i64 = 25;
    for iter in 0..ITERS {
        let session = format!("s-{iter}");
        let nav_id = iter * 2 + 1;
        let eval_id = iter * 2 + 2;

        // Pipeline BOTH frames without awaiting the (slow) navigate response.
        send_json(&mut framed, &web_request(nav_id, "web.navigate", &session)).await;
        send_json(&mut framed, &web_request(eval_id, "web.evaluate", &session)).await;

        // Collect both responses (correlate by id; the slow navigate's may
        // arrive after the evaluate's on the wire — execution order is what
        // we pin, not delivery order).
        let mut by_id = std::collections::HashMap::new();
        for _ in 0..2 {
            let resp = recv_json(&mut framed).await;
            let id = resp.get("id").and_then(|v| v.as_i64()).unwrap();
            by_id.insert(id, resp);
        }

        let eval = &by_id[&eval_id];
        assert_ne!(
            error_code(eval),
            "too_many_requests",
            "iter {iter}: pipelined same-session action must queue, not be rejected; got: {eval}"
        );
        assert_eq!(
            eval["result"]["navigated_first"],
            serde_json::json!(true),
            "iter {iter}: evaluate must observe the navigate that preceded it; got: {eval}"
        );
        let nav = &by_id[&nav_id];
        assert_eq!(
            nav["result"]["navigated"],
            serde_json::json!(true),
            "iter {iter}: navigate must complete; got: {nav}"
        );

        // The router's enter order for THIS session must be [navigate, evaluate]
        // — the WAL/receipt-append-order proxy.
        let order: Vec<String> = state
            .lock()
            .unwrap()
            .enter_log
            .iter()
            .filter(|(s, _)| s == &session)
            .map(|(_, m)| m.clone())
            .collect();
        assert_eq!(
            order,
            vec!["web.navigate".to_string(), "web.evaluate".to_string()],
            "iter {iter}: same-session dispatch entered out of submission order: {order:?}"
        );
    }
}

/// Two DIFFERENT-session surface verbs pipelined on ONE connection still
/// run concurrently: distinct sessions get distinct ordered workers, so a
/// slow `web.navigate` on session A does not delay one on session B. Proven
/// by overlapping dispatch windows (serialization would force B to start
/// only after A finished).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn different_session_actions_on_one_connection_stay_concurrent() {
    let state = Arc::new(Mutex::new(OrderingState::default()));
    let router = Arc::new(OrderingRouter {
        state: Arc::clone(&state),
        nav_delay: Duration::from_millis(400),
    });
    let (srv, _bg) = start_server_with_router(router).await;
    let mut framed = connect_and_hello(&srv).await;

    send_json(&mut framed, &web_request(1, "web.navigate", "sess-A")).await;
    send_json(&mut framed, &web_request(2, "web.navigate", "sess-B")).await;

    // Drain both responses.
    for _ in 0..2 {
        let _ = recv_json(&mut framed).await;
    }

    let windows = state.lock().unwrap().windows.clone();
    let a = windows
        .iter()
        .find(|w| w.session == "sess-A" && w.method == "web.navigate")
        .expect("session A navigate window");
    let b = windows
        .iter()
        .find(|w| w.session == "sess-B" && w.method == "web.navigate")
        .expect("session B navigate window");

    // Overlap test: A.enter < B.exit && B.enter < A.exit. With both delays
    // at 400 ms and concurrent dispatch the windows overlap heavily; under
    // (buggy) cross-session serialization B would start only after A's exit.
    assert!(
        a.enter < b.exit && b.enter < a.exit,
        "cross-session navigates must overlap (concurrent); \
         A=[{:?},{:?}] B=[{:?},{:?}]",
        a.enter,
        a.exit,
        b.enter,
        b.exit,
    );
}

/// The ORDERED lane must NOT block the read loop: a same-session pipeline
/// deeper than the worker queue still leaves `request.cancel` / `daemon.hello`
/// answerable on the fast lane (a blocking enqueue would freeze the read loop
/// behind a slow head-of-line dispatch). Flood one session with slow
/// navigates, then pipeline a `daemon.hello`; the ack must come back well
/// before the head navigate could finish, and the overflow past the queue
/// depth must answer `too_many_requests` rather than hang.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ordered_lane_flood_does_not_block_fast_lane() {
    let state = Arc::new(Mutex::new(OrderingState::default()));
    let router = Arc::new(OrderingRouter {
        state: Arc::clone(&state),
        // Long enough that, under a blocking enqueue, the fast-lane ack would
        // be stuck for seconds; the assertion bound (700 ms) sits well under it.
        nav_delay: Duration::from_millis(1500),
    });
    let (srv, _bg) = start_server_with_router(router).await;
    let mut framed = connect_and_hello(&srv).await;

    // Flood far past the queue depth (default 8) + the in-flight slot.
    const FLOOD: i64 = 20;
    for i in 1..=FLOOD {
        send_json(&mut framed, &web_request(i, "web.navigate", "flood")).await;
    }
    // The fast-lane probe, pipelined LAST behind the whole flood.
    let hello_id: i64 = 9999;
    send_json(
        &mut framed,
        &request(hello_id.into(), "daemon.hello", serde_json::json!({})),
    )
    .await;

    // Read responses until the hello ack appears, measuring how long it took.
    // With the non-blocking ordered enqueue, the read loop processes the whole
    // flood (queuing some, rejecting the overflow) and the hello inline — all
    // before the 1.5 s head navigate completes.
    let started = Instant::now();
    let mut saw_too_many = false;
    let mut hello_latency = None;
    for _ in 0..(FLOOD + 1) {
        let resp = tokio::time::timeout(Duration::from_millis(700), recv_json(&mut framed))
            .await
            .expect("fast-lane ack must arrive promptly; read loop is blocked");
        if error_code(&resp) == "too_many_requests" {
            saw_too_many = true;
        }
        if resp.get("id").and_then(|v| v.as_i64()) == Some(hello_id) {
            assert_eq!(resp["result"]["hello"], serde_json::json!("ok"));
            hello_latency = Some(started.elapsed());
            break;
        }
    }

    let latency = hello_latency.expect("daemon.hello must be answered");
    assert!(
        latency < Duration::from_millis(700),
        "daemon.hello fast lane must not queue behind the ordered flood; took {latency:?}"
    );
    assert!(
        saw_too_many,
        "flood past the queue depth must reject overflow with too_many_requests, not buffer/hang"
    );
}
