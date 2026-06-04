//! loom-mcp ↔ daemon resilience reproduce-first tests.
//!
//! These drive the long-lived `loom_mcp::rpc_client::RpcClient` (the same
//! persistent-connection client the `loom-mcp` stdio server holds for its whole
//! lifetime) against a real `loom-daemon` via [`DaemonTestHarness`]. They
//! reproduce the two reported symptoms — `"malformed error response"` and the
//! broken-pipe on a dropped persistent connection — and assert the post-fix
//! recovered behaviour.
//!
//! Daemon-backed, so `#[ignore]`d like the other e2e tests (run in CI with
//! `--include-ignored`). Single bounded waits, no retry loops (FND-0006).

#![cfg(unix)]

mod common;

use std::sync::Arc;

use common::daemon_test_harness::DaemonTestHarness;
use loom_mcp::rpc_client::{RpcClient, RpcClientConfig};
use loom_shared::error_format::LoomErrorCode;
use serde_json::json;

/// Build a persistent MCP `RpcClient` wired to the harness's socket and a
/// token file we control (so a daemon restart that rotates the token is handled
/// by re-pointing the same path — the client re-reads it on every `connect()`).
fn token_file(harness: &DaemonTestHarness) -> std::path::PathBuf {
    let path = harness.home().join("test-hello.token");
    let token = harness
        .hello_token()
        .expect("harness started — token present");
    std::fs::write(&path, token).expect("write test token file");
    path
}

async fn connected_client(harness: &DaemonTestHarness) -> Arc<RpcClient> {
    let mut cfg = RpcClientConfig::defaults();
    cfg.socket_path = harness.socket_path().to_path_buf();
    cfg.hello_token_path = token_file(harness);
    let obs = loom_mcp::mcp_observability::init_subscriber(false);
    let rpc = RpcClient::new(cfg, obs);
    rpc.connect()
        .await
        .expect("initial connect to harness daemon");
    rpc
}

/// T1 — RED reproduce: a daemon error whose canonical code is multi-word
/// (`session-not-found`) must round-trip to the MCP client as that typed code,
/// NOT collapse to `code=io, message="malformed error response"`.
///
/// RED today: daemon emits snake_case `session_not_found`; the client
/// deserializes into the kebab-case canonical enum, the parse fails, and it
/// falls back to `from_rpc_io("malformed error response")`.
/// GREEN after WI-1 (round-trip repair).
#[tokio::test]
#[ignore = "spawns a real loom-daemon; run with --include-ignored"]
async fn multiword_error_code_round_trips_not_malformed() {
    let mut harness = DaemonTestHarness::new();
    harness.start();
    let rpc = connected_client(&harness).await;

    let err = rpc
        .call(
            "session.close",
            json!({ "session_id": "loom-sess-does-not-exist" }),
        )
        .await
        .expect_err("closing a non-existent session must be an error");

    assert_ne!(
        err.message, "malformed error response",
        "error round-trip is broken: the daemon's typed code failed to deserialize \
         MCP-side (got code={:?})",
        err.code
    );
    assert_ne!(
        err.code,
        LoomErrorCode::Io,
        "a missing-session error must surface as a typed code (e.g. session-not-found), \
         not opaque io; got message={:?}",
        err.message
    );
}

/// T2 — RED reproduce: a dropped persistent connection must auto-recover.
/// A long-lived client whose daemon went away and came back (the idle-drop /
/// daemon-restart scenario) must transparently reconnect and satisfy the next
/// action — never surface a raw broken-pipe `io` error.
///
/// RED today: the first call after the drop returns `code=io` (broken pipe /
/// connection closed); `RpcClient::call` only reconnects in the background and
/// returns the original error. GREEN after WI-3/WI-4 (keepalive + reconnect-and
/// -retry-once).
#[tokio::test]
#[ignore = "spawns a real loom-daemon; run with --include-ignored"]
async fn dropped_persistent_connection_auto_recovers() {
    let mut harness = DaemonTestHarness::new();
    harness.start();
    let rpc = connected_client(&harness).await;

    // Baseline: the persistent connection works.
    rpc.call("health.ping", json!({}))
        .await
        .expect("baseline ping over the fresh persistent connection");

    // Drop the connection out from under the client, then bring the daemon
    // back on the SAME socket (new token → re-point the token file the client
    // reads on reconnect).
    harness.stop();
    harness.start();
    let path = harness.home().join("test-hello.token");
    std::fs::write(&path, harness.hello_token().expect("restarted token"))
        .expect("rewrite token after restart");

    // The next action must succeed via transparent reconnect+retry.
    let res = rpc.call("health.ping", json!({})).await;
    assert!(
        res.is_ok(),
        "a dropped persistent connection must auto-recover, got {:?}",
        res.err()
    );
}

/// T6 — REAL-Chromium soak (release / nightly only). Drives a long-lived MCP
/// `RpcClient` through ≥200 sequential `session.create → web.navigate →
/// web.screenshot → web.clear_cookies → session.close` cycles against a
/// REAL, already-running, fully-provisioned daemon (chromium + AOT surfaces),
/// asserting ZERO broken-pipe / malformed / transport_dropped errors over the
/// sustained run. Opt-in via `LOOM_SOAK_REAL=1` (else it skips), because a
/// hermetic test daemon has no real browser or AOT surfaces. Point it at the
/// daemon with `LOOM_SOCKET_PATH` (defaults to the platform socket).
#[tokio::test]
#[ignore = "real-Chromium release soak; set LOOM_SOAK_REAL=1 against a running daemon"]
async fn real_chromium_soak_200_sessions_no_transport_errors() {
    if std::env::var("LOOM_SOAK_REAL").ok().as_deref() != Some("1") {
        eprintln!("skipping real soak: set LOOM_SOAK_REAL=1 (needs a provisioned daemon)");
        return;
    }
    let n: usize = std::env::var("LOOM_SOAK_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);

    let mut cfg = RpcClientConfig::defaults();
    if let Ok(sock) = std::env::var("LOOM_SOCKET_PATH") {
        cfg.socket_path = sock.into();
    }
    let obs = loom_mcp::mcp_observability::init_subscriber(false);
    let rpc = RpcClient::new(cfg, obs);
    rpc.connect().await.expect("connect to real daemon");

    let is_transport_error = |e: &loom_rpc::error::LoomError| {
        matches!(e.code, LoomErrorCode::TransportDropped | LoomErrorCode::Io)
            || e.message.contains("malformed error response")
            || e.message.contains("Broken pipe")
    };

    for i in 0..n {
        let created = rpc
            .call("session.create", json!({ "profile": "standard" }))
            .await
            .unwrap_or_else(|e| panic!("cycle {i}: session.create: {e}"));
        let sid = created
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("cycle {i}: no session_id"))
            .to_string();

        for (verb, params) in [
            (
                "web.navigate",
                json!({ "session": sid, "url": "about:blank" }),
            ),
            ("web.screenshot", json!({ "session": sid })),
            ("web.clear_cookies", json!({ "session": sid })),
        ] {
            if let Err(e) = rpc.call(verb, params).await {
                assert!(
                    !is_transport_error(&e),
                    "cycle {i}: {verb} returned a transport/flake error: {e}"
                );
            }
        }
        let _ = rpc
            .call("session.close", json!({ "session_id": sid }))
            .await;
    }
    eprintln!("real soak: {n} sessions completed with no transport errors");
}
