// Concurrent stdio dispatch (audit 2026-06-10): the request loop used to
// await each dispatch inline, so one long tools/call head-of-line blocked
// ping, initialize, etc. The loop now spawns one bounded task per frame
// and serializes only the stdout writes; MCP correlates responses by id,
// so out-of-order responses are legal.
//
// Fixture: StdioTransport over in-memory duplex pipes with a scripted
// dispatch closure — no daemon, no real stdio.
//
// Run: cargo test -p loom-mcp --test stdio_concurrency

use loom_mcp::stdio_transport::{
    Dispatch, McpRequest, McpResponse, StdioTransport, MAX_CONCURRENT_REQUESTS,
};
use std::sync::Arc;
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
