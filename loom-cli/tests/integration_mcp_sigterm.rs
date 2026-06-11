//! Full-process SIGTERM coverage for the real `loom-mcp` binary: a SIGTERM
//! must close the implicit session it created (clean daemon-side
//! `SessionTerminal` in the WAL) and exit — not leak the session and require a
//! SIGKILL.
//!
//! The existing loom-mcp signal tests
//! (`mcp_main::interface_tests::signal_delivery_cancels_shutdown_token`,
//! `serve_until_shutdown_exits_on_cancel_while_stdin_open`, etc.) are
//! IN-PROCESS: they prove the token-cancel plumbing, but NOT that the real
//! `loom-mcp` binary, end to end, (a) installs the signal handler on its serve
//! loop, (b) runs `dispatcher.close_implicit_session()` on the way out
//! (`mcp_main::run` teardown), and (c) that close reaches the daemon and lands
//! a `SessionTerminal` in that session's WAL.
//!
//! This test wires the REAL `loom-daemon` (DaemonTestHarness) to the REAL
//! `loom-mcp` (subprocess over stdio), creates the implicit session via a
//! `loom.session.info` tool call (no chromium needed — `session.create` is a
//! plain RPC), SIGTERMs the mcp process, and asserts the WAL gets a
//! `session_terminal` entry.
//!
//! Failure mode if the close-on-shutdown path were un-wired: if SIGTERM only
//! cancelled the loop without running `close_implicit_session` (the historical
//! bug where signals stored into a dead flag), no `session_terminal` would
//! appear and the WAL assertion fails. If the daemon never received the close,
//! same outcome.
//!
//! `#[ignore]`d like the other binary-spawning lifecycle tests; CI runs it
//! under `--include-ignored`.

#![cfg(unix)]

mod common;

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use common::daemon_test_harness::DaemonTestHarness;

/// Spawn `loom-mcp serve` over stdio, wired to the daemon `socket`. The daemon
/// pins its data_root (so we can locate the WAL); the MCP reads the daemon's
/// HELLO token from that same data_root via `LOOM_HELLO_TOKEN_PATH` (the
/// RpcClient otherwise defaults to `dirs::data_dir()/loom/auth/hello.token`,
/// which would not match the pinned data_root).
fn spawn_mcp(socket: &Path, home: &Path, token_path: &Path) -> (Child, BufReader<ChildStdout>) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_loom-mcp"));
    cmd.arg("serve");
    cmd.env("HOME", home)
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_RUNTIME_DIR", home)
        .env("LOOM_KEYCHAIN_BACKEND", "in_memory")
        .env("LOOM_SOCKET_PATH", socket)
        .env("LOOM_HELLO_TOKEN_PATH", token_path)
        .env("RUST_LOG", "warn")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().expect("spawn loom-mcp");
    let stdout = child.stdout.take().expect("mcp stdout");
    (child, BufReader::new(stdout))
}

fn send_rpc(child: &mut Child, method: &str, params: serde_json::Value, id: u64) {
    let req = serde_json::json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
    let stdin = child.stdin.as_mut().expect("mcp stdin");
    writeln!(stdin, "{}", serde_json::to_string(&req).unwrap()).expect("write rpc");
    stdin.flush().expect("flush");
}

/// Read newline-delimited JSON-RPC responses until one with `id == want_id`
/// arrives (notifications / other ids are skipped). `None` on EOF/timeout.
fn read_rpc_id(
    reader: &mut BufReader<ChildStdout>,
    want_id: u64,
    timeout: Duration,
) -> Option<serde_json::Value> {
    let start = Instant::now();
    let mut line = String::new();
    loop {
        if start.elapsed() > timeout {
            return None;
        }
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return None,
            Ok(_) => {
                let t = line.trim();
                if t.is_empty() {
                    continue;
                }
                let v: serde_json::Value = serde_json::from_str(t).ok()?;
                if v.get("id").and_then(|i| i.as_u64()) == Some(want_id) {
                    return Some(v);
                }
                // else: a different id / notification; keep reading.
            }
            Err(_) => return None,
        }
    }
}

/// Pull the `session_id` out of a `loom.session.info` tools/call result. The
/// MCP tool-result envelope wraps the JSON payload in a text content block.
fn session_id_from_tool_result(resp: &serde_json::Value) -> Option<String> {
    let text = resp
        .pointer("/result/content/0/text")
        .and_then(|v| v.as_str())?;
    let parsed: serde_json::Value = serde_json::from_str(text).ok()?;
    parsed
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

/// True once the session's WAL contains a `session_terminal` entry.
fn wal_has_session_terminal(data_root: &Path, session_id: &str) -> bool {
    let wal = data_root
        .join("sessions")
        .join(session_id)
        .join("manifest.wal");
    let Ok(contents) = std::fs::read_to_string(&wal) else {
        return false;
    };
    // The manifest entries are tagged with snake_case `session_terminal`
    // (ManifestEntry serde rename). A substring check over the WAL text is
    // sufficient and robust to the exact JSON framing.
    contents.contains("session_terminal")
}

#[test]
#[ignore = "spawns a real loom-daemon + loom-mcp; run under `cargo test -- --include-ignored`"]
fn mcp_sigterm_closes_implicit_session_and_exits() {
    // Real daemon with a pinned data_root (so we can locate the WAL).
    let (mut daemon, data_root) = DaemonTestHarness::new().with_data_root_under_home();
    daemon.start();
    let home = daemon.home().to_path_buf();
    let token_path = data_root.join("auth").join("hello.token");

    // Real loom-mcp wired to the daemon.
    let (mut mcp, mut reader) = spawn_mcp(daemon.socket_path(), &home, &token_path);

    // MCP handshake.
    send_rpc(
        &mut mcp,
        "initialize",
        serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "sigterm-test", "version": "0"}
        }),
        1,
    );
    let init = read_rpc_id(&mut reader, 1, Duration::from_secs(15))
        .expect("MCP must answer initialize (is the daemon up?)");
    assert!(
        init.get("result").is_some(),
        "initialize must succeed: {init}"
    );

    // Create the implicit session via loom.session.info (routed before the tool
    // cache gate; no chromium required).
    send_rpc(
        &mut mcp,
        "tools/call",
        serde_json::json!({"name": "loom.session.info", "arguments": {}}),
        2,
    );
    let info = read_rpc_id(&mut reader, 2, Duration::from_secs(15))
        .expect("MCP must answer loom.session.info");
    let session_id = session_id_from_tool_result(&info)
        .unwrap_or_else(|| panic!("session.info must yield a session_id, got: {info}"));
    assert!(!session_id.is_empty(), "implicit session id non-empty");

    // The session exists; its WAL must NOT yet have a terminal entry.
    assert!(
        !wal_has_session_terminal(&data_root, &session_id),
        "session must be live (no SessionTerminal) before SIGTERM"
    );

    // SIGTERM the real loom-mcp process — the production graceful path.
    let pid = mcp.id() as libc::pid_t;
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }

    // The process must exit (graceful, not requiring SIGKILL) within the bound.
    let exited = {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match mcp.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() >= deadline => break None,
                Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                Err(e) => panic!("try_wait failed: {e}"),
            }
        }
    };
    let status = exited.unwrap_or_else(|| {
        let _ = mcp.kill();
        let _ = mcp.wait();
        panic!("loom-mcp did not exit within 15s of SIGTERM — signal handler not wired");
    });
    assert!(
        status.success() || status.code().is_none(),
        "mcp exit after SIGTERM should be orderly (Ok teardown), got: {status:?}"
    );

    // The close runs in run()'s teardown AFTER the serve loop unwinds; give the
    // daemon a brief window to commit the SessionTerminal WAL append.
    let mut terminal_seen = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if wal_has_session_terminal(&data_root, &session_id) {
            terminal_seen = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        terminal_seen,
        "SIGTERM must close the implicit session: no SessionTerminal in WAL for {session_id} \
         under {} — close_implicit_session() did not run / reach the daemon",
        data_root.display()
    );

    // daemon harness Drop stops the daemon.
    let _ = mcp; // already reaped
}
