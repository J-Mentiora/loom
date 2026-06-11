// Concurrent stdio dispatch (audit 2026-06-10): the request loop used to
// await each dispatch inline, so one long tools/call head-of-line blocked
// ping, initialize, etc. The loop now classifies each frame onto two lanes
// that funnel into a single stdout writer:
//   - CONCURRENT (control-plane: ping, initialize, tools/list, …): one
//     bounded task per frame, so a slow action can't head-of-line block.
//   - ORDERED (session-mutating tools/call: loom.web.*, loom.session.reset):
//     a single worker awaits each dispatch in turn, so a navigate the client
//     sent before an evaluate ALWAYS executes first (the implicit session is
//     one browser; out-of-order execution read the pre-navigate page — the
//     v0.11.0-blocking regression from PR #162's per-frame spawn).
// MCP correlates responses by id, so out-of-order RESPONSE delivery is legal;
// what these tests pin is EXECUTION order of session-mutating actions.
//
// Fixture: StdioTransport over in-memory duplex pipes with a scripted
// dispatch closure — no daemon, no real stdio.
//
// Run: cargo test -p loom-mcp --test stdio_concurrency

use loom_mcp::stdio_transport::{
    Dispatch, McpRequest, McpResponse, StdioTransport, MAX_CONCURRENT_REQUESTS,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Dispatch closure: method "slow" sleeps before answering; everything
/// else answers immediately. Responses echo the request id.
fn scripted_dispatch(slow_delay: Duration) -> Dispatch {
    Arc::new(move |req: McpRequest| {
        Box::pin(async move {
            if req.method == "slow" {
                tokio::time::sleep(slow_delay).await;
            }
            req.id.map(|id| McpResponse {
                jsonrpc: "2.0".into(),
                id,
                result: Some(serde_json::json!({ "method": req.method })),
                error: None,
            })
        }) as futures::future::BoxFuture<'static, Option<McpResponse>>
    })
}

fn frame(id: u64, method: &str) -> String {
    format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"{method}\",\"params\":{{}}}}\n")
}

/// A slow tools/call must not head-of-line block ping: the ping response
/// (sent AFTER the slow request) must come back FIRST. Pre-fix the serial
/// loop always answered in request order, so this deterministically fails
/// without the concurrency fix — no wall-clock assertions needed.
#[tokio::test]
async fn slow_call_does_not_block_ping() {
    let (mut stdin_writer, stdin_reader) = tokio::io::duplex(64 * 1024);
    let (stdout_writer, stdout_reader) = tokio::io::duplex(64 * 1024);
    let transport = StdioTransport::with_io(stdin_reader, stdout_writer);
    let server = tokio::spawn(transport.run(scripted_dispatch(Duration::from_millis(500))));

    stdin_writer
        .write_all(frame(1, "slow").as_bytes())
        .await
        .unwrap();
    stdin_writer
        .write_all(frame(2, "ping").as_bytes())
        .await
        .unwrap();

    let mut lines = BufReader::new(stdout_reader).lines();
    let first: McpResponse = serde_json::from_str(
        &tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .expect("first response within budget")
            .unwrap()
            .expect("stream open"),
    )
    .unwrap();
    assert_eq!(
        first.id,
        serde_json::json!(2),
        "ping (id 2) must answer before the slow call (id 1)"
    );
    let second: McpResponse = serde_json::from_str(
        &tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .expect("slow response within budget")
            .unwrap()
            .expect("stream open"),
    )
    .unwrap();
    assert_eq!(second.id, serde_json::json!(1));

    drop(stdin_writer); // EOF → orderly exit
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("run() exits on EOF")
        .unwrap()
        .expect("orderly exit");
}

/// A flood larger than the concurrency bound must not drop or duplicate
/// responses: every request gets exactly one response (the semaphore
/// applies backpressure instead of unbounded task spawn).
#[tokio::test]
async fn flood_beyond_concurrency_bound_answers_every_request() {
    let total = MAX_CONCURRENT_REQUESTS + 8;
    let (mut stdin_writer, stdin_reader) = tokio::io::duplex(256 * 1024);
    let (stdout_writer, stdout_reader) = tokio::io::duplex(256 * 1024);
    let transport = StdioTransport::with_io(stdin_reader, stdout_writer);
    let server = tokio::spawn(transport.run(scripted_dispatch(Duration::from_millis(1))));

    for id in 0..total {
        stdin_writer
            .write_all(frame(id as u64, "slow").as_bytes())
            .await
            .unwrap();
    }
    drop(stdin_writer); // EOF: in-flight responses must still drain

    let mut seen = vec![false; total];
    let mut lines = BufReader::new(stdout_reader).lines();
    for _ in 0..total {
        let resp: McpResponse = serde_json::from_str(
            &tokio::time::timeout(Duration::from_secs(10), lines.next_line())
                .await
                .expect("response within budget")
                .unwrap()
                .expect("stream open before all responses arrived"),
        )
        .unwrap();
        let id = resp.id.as_u64().expect("numeric id") as usize;
        assert!(!seen[id], "duplicate response for id {id}");
        seen[id] = true;
    }
    assert!(seen.iter().all(|s| *s), "every request must be answered");
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("run() exits after drain")
        .unwrap()
        .expect("orderly exit");
}

/// EOF with a dispatch still in flight: the response must be drained to
/// stdout before run() returns (the writer task waits for all sender
/// clones, including the in-flight task's, to drop).
#[tokio::test]
async fn eof_drains_in_flight_responses_before_returning() {
    let (mut stdin_writer, stdin_reader) = tokio::io::duplex(64 * 1024);
    let (stdout_writer, stdout_reader) = tokio::io::duplex(64 * 1024);
    let transport = StdioTransport::with_io(stdin_reader, stdout_writer);
    let server = tokio::spawn(transport.run(scripted_dispatch(Duration::from_millis(200))));

    stdin_writer
        .write_all(frame(7, "slow").as_bytes())
        .await
        .unwrap();
    drop(stdin_writer); // immediate EOF while the dispatch sleeps

    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("run() exits after drain")
        .unwrap()
        .expect("orderly exit");
    // The response was written before run() returned.
    let mut lines = BufReader::new(stdout_reader).lines();
    let resp: McpResponse =
        serde_json::from_str(&lines.next_line().await.unwrap().expect("drained response")).unwrap();
    assert_eq!(resp.id, serde_json::json!(7));
}

/// Parse errors still get the JSON-RPC -32700 response, interleaved
/// safely with concurrent dispatches.
#[tokio::test]
async fn parse_error_still_answered_under_concurrency() {
    let (mut stdin_writer, stdin_reader) = tokio::io::duplex(64 * 1024);
    let (stdout_writer, stdout_reader) = tokio::io::duplex(64 * 1024);
    let transport = StdioTransport::with_io(stdin_reader, stdout_writer);
    let server = tokio::spawn(transport.run(scripted_dispatch(Duration::from_millis(100))));

    stdin_writer
        .write_all(frame(1, "slow").as_bytes())
        .await
        .unwrap();
    stdin_writer.write_all(b"{not json\n").await.unwrap();

    let mut lines = BufReader::new(stdout_reader).lines();
    let first: McpResponse = serde_json::from_str(
        &tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .expect("parse-error response within budget")
            .unwrap()
            .expect("stream open"),
    )
    .unwrap();
    let err = first.error.expect("parse error must carry an error");
    assert_eq!(err.code, -32700);

    drop(stdin_writer);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("run() exits on EOF")
        .unwrap()
        .expect("orderly exit");
}

// === Session-mutating execution ordering (v0.11.0 regression) ===

/// A `tools/call` frame for a web verb (`loom.web.<verb>`).
fn web_call_frame(id: u64, verb: &str) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"tools/call\",\
         \"params\":{{\"name\":\"loom.web.{verb}\",\"arguments\":{{}}}}}}\n"
    )
}

/// Dispatch that models the implicit session as ONE browser: it records
/// each web action's EFFECT order — i.e. when the action FINISHES touching
/// the page — and sleeps `first_delay` inside the FIRST web call it sees
/// (the slow `navigate` from the e2e race). Recording at completion is what
/// reflects effect ordering against the single browser: under a concurrent
/// loop the fast `evaluate` finishes its effect before the slow `navigate`
/// (the bug → recorded `[evaluate, navigate]`); a FIFO worker runs `navigate`
/// to completion before `evaluate` starts (→ `[navigate, evaluate]`).
/// `tools/call` records the tool name; `ping` answers `{}` and is not
/// recorded (it is control-plane, not a session action).
fn ordering_dispatch(first_delay: Duration) -> (Dispatch, Arc<Mutex<Vec<String>>>) {
    let effect_order = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen_first = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let order_for_closure = effect_order.clone();
    let dispatch: Dispatch = Arc::new(move |req: McpRequest| {
        let effect_order = order_for_closure.clone();
        let seen_first = seen_first.clone();
        Box::pin(async move {
            let tool_name = if req.method == "tools/call" {
                req.params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            } else {
                None
            };
            let is_web = tool_name
                .as_deref()
                .is_some_and(|n| n.starts_with("loom.web."));
            // The first web action holds its dispatch open, giving a racing
            // later action every chance to overtake it under a concurrent
            // loop.
            let is_first_web = is_web
                && seen_first
                    .compare_exchange(
                        false,
                        true,
                        std::sync::atomic::Ordering::SeqCst,
                        std::sync::atomic::Ordering::SeqCst,
                    )
                    .is_ok();
            if is_first_web {
                tokio::time::sleep(first_delay).await;
            }
            // Record the EFFECT order: web actions only, at completion.
            if let Some(name) = tool_name.filter(|_| is_web) {
                effect_order.lock().unwrap().push(name);
            }
            req.id.map(|id| McpResponse {
                jsonrpc: "2.0".into(),
                id,
                result: Some(serde_json::json!({})),
                error: None,
            })
        }) as futures::future::BoxFuture<'static, Option<McpResponse>>
    });
    (dispatch, effect_order)
}

/// REGRESSION: a `navigate` followed by an `evaluate` (the e2e race) must
/// take EFFECT in submission order even though the navigate is slow. Pre-fix
/// the per-frame spawn let the fast evaluate finish (read the page) before
/// the slow navigate changed it. The slow navigate's dispatch holds open for
/// 400ms; under the concurrent spawn lane the evaluate would complete — and
/// be recorded — first. Asserting the recorded EFFECT order pins the fix.
#[tokio::test]
async fn navigate_executes_before_evaluate_in_submission_order() {
    let (dispatch, effect_order) = ordering_dispatch(Duration::from_millis(400));
    let (mut stdin_writer, stdin_reader) = tokio::io::duplex(64 * 1024);
    let (stdout_writer, stdout_reader) = tokio::io::duplex(64 * 1024);
    let transport = StdioTransport::with_io(stdin_reader, stdout_writer);
    let server = tokio::spawn(transport.run(dispatch));

    // Fire the two session-mutating calls back-to-back.
    stdin_writer
        .write_all(web_call_frame(3, "navigate").as_bytes())
        .await
        .unwrap();
    stdin_writer
        .write_all(web_call_frame(4, "evaluate").as_bytes())
        .await
        .unwrap();
    drop(stdin_writer); // EOF → orderly drain

    // Drain both responses (order on the wire is irrelevant).
    let mut lines = BufReader::new(stdout_reader).lines();
    for _ in 0..2 {
        let _ = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .expect("response within budget")
            .unwrap()
            .expect("stream open");
    }
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("run() exits after drain")
        .unwrap()
        .expect("orderly exit");

    let order = effect_order.lock().unwrap().clone();
    assert_eq!(
        order,
        vec![
            "loom.web.navigate".to_string(),
            "loom.web.evaluate".to_string()
        ],
        "session-mutating actions must take EFFECT in submission order \
         (navigate before evaluate); got {order:?}"
    );
}

/// The concurrency benefit (#162's goal) is preserved: a `ping` sent AFTER a
/// slow session-mutating action must answer WITHOUT waiting for that action.
/// The ordered action holds its dispatch open for 600ms; the ping rides the
/// concurrent lane, so its response must come back first. Pre-#162 (fully
/// serial) the ping waited behind the action and this would fail.
#[tokio::test]
async fn ping_not_blocked_by_slow_ordered_action() {
    let (dispatch, _effect_order) = ordering_dispatch(Duration::from_millis(600));
    let (mut stdin_writer, stdin_reader) = tokio::io::duplex(64 * 1024);
    let (stdout_writer, stdout_reader) = tokio::io::duplex(64 * 1024);
    let transport = StdioTransport::with_io(stdin_reader, stdout_writer);
    let server = tokio::spawn(transport.run(dispatch));

    stdin_writer
        .write_all(web_call_frame(3, "navigate").as_bytes())
        .await
        .unwrap();
    stdin_writer
        .write_all(frame(99, "ping").as_bytes())
        .await
        .unwrap();

    let mut lines = BufReader::new(stdout_reader).lines();
    let first: McpResponse = serde_json::from_str(
        &tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .expect("first response within budget")
            .unwrap()
            .expect("stream open"),
    )
    .unwrap();
    assert_eq!(
        first.id,
        serde_json::json!(99),
        "ping (id 99) must answer before the slow ordered navigate (id 3)"
    );

    drop(stdin_writer); // EOF → orderly drain of the slow action
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("run() exits after drain")
        .unwrap()
        .expect("orderly exit");
}
