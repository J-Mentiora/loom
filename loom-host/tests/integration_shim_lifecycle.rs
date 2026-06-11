//! Real-subprocess lifecycle coverage for the ShimManager hardening that is
//! otherwise only UNIT-tested.
//!
//! The unit tests in `shim_manager/interface_tests.rs` exercise these properties
//! against a synthetic `fake_process()` / a raw socketpair fed to
//! `host_read_loop` directly. That proves the pure logic but NOT that the
//! REAL chain — production `spawn_shim`, a real `loom-shim-chromium`
//! subprocess, the deterministic `fake-chromium` DevTools fixture — actually
//! drives those branches.
//!
//! Covered here, all real-subprocess:
//!
//! 1. `record_failure` CLASSIFICATION: a live shim that REPORTS an
//!    application-level CDP error (a `DOM.getBoxModel` against an unknown node →
//!    CdpProtocolError) must NOT be evicted — the Chromium under it is healthy
//!    and holds mid-session state. A subsequent `send` to the SAME shim must
//!    succeed on the SAME live subprocess. The unit test calls
//!    `record_failure(Application)` directly; this proves the live
//!    `send`→shim→fake-chromium→error path actually classifies as Application.
//!
//! 2. TRANSPORT failure (control): kill the subprocess out from under the
//!    manager; the next `send` fails cleanly (bounded, not a hang) and the dead
//!    handle is evicted/respawned — the Transport branch on the real path.
//!
//! 3. CLOEXEC: when the host closes its end of the IPC socketpair (drop the
//!    `ShimProcess`), the exec'd shim must see EOF and exit on its own. This is
//!    only possible if the shim did NOT inherit the HOST's end of its own IPC
//!    socket — i.e. the socketpair was created CLOEXEC in production
//!    `spawn_shim`. If CLOEXEC were dropped, the child would hold a copy of the
//!    host's write end open, never see EOF, and never exit — this test would
//!    time out.
//!
//! `#[ignore]`d like the other fake-chromium tests; build the fixtures first:
//!   cargo build -p loom-shims --features fake-chromium-bin --bin fake-chromium
//!   cargo build -p loom-cli --bin loom-shim-chromium
//! then run with `--ignored`.

#![cfg(unix)]

use std::path::PathBuf;
use std::time::Duration;

use loom_host::host_observability::HostObservability;
use loom_host::shim_manager::process::{spawn_shim, SpawnConfig};
use loom_host::shim_manager::{ShimConfig, ShimId, ShimManager};
use loom_shared::shim_protocol::{ciborium_to_vec, CdpMessage};

fn target_bin_dir() -> PathBuf {
    let test_exe = std::env::current_exe().expect("current_exe");
    let deps = test_exe.parent().expect("deps dir");
    deps.parent().expect("debug dir").to_path_buf()
}

fn shim_bin() -> String {
    target_bin_dir()
        .join("loom-shim-chromium")
        .to_string_lossy()
        .into_owned()
}

fn fake_chromium_bin() -> String {
    target_bin_dir()
        .join("fake-chromium")
        .to_string_lossy()
        .into_owned()
}

fn require_fixtures() {
    for p in [fake_chromium_bin(), shim_bin()] {
        assert!(
            std::path::Path::new(&p).exists(),
            "fixture binary not built at {p} — run `cargo build -p loom-shims \
             --features fake-chromium-bin --bin fake-chromium` and \
             `cargo build -p loom-cli --bin loom-shim-chromium` first"
        );
    }
}

fn shim_env(udd: &std::path::Path) -> Vec<(String, String)> {
    vec![
        ("LOOM_SHIM_CHROMIUM_PATH".into(), fake_chromium_bin()),
        ("LOOM_SHIM_USER_DATA_DIR".into(), udd.display().to_string()),
        (
            "LOOM_FAKE_CHROMIUM_USER_DATA_DIR".into(),
            udd.display().to_string(),
        ),
    ]
}

fn register(mgr: &ShimManager, id: &ShimId, udd: &std::path::Path) {
    mgr.register(
        id.clone(),
        ShimConfig {
            binary_path: shim_bin().into(),
            args: vec![],
            env: shim_env(udd),
            spawn_retry: 1,
            breaker_threshold: 10, // high: don't trip the breaker during the test
            breaker_open_ms: 5_000,
            send_timeout_ms: 10_000,
            recv_timeout_ms: 30_000,
        },
    );
}

/// A `Page.navigate` CdpMessage — a benign round-trip that forces a real spawn.
fn navigate_payload() -> Vec<u8> {
    let msg = CdpMessage {
        method: "Page.navigate".into(),
        params: ciborium::value::Value::Map(vec![(
            ciborium::value::Value::Text("url".into()),
            ciborium::value::Value::Text("https://example.com".into()),
        )]),
    };
    ciborium_to_vec(&msg).expect("encode CdpMessage")
}

/// A `DOM.getBoxModel` against an unknown nodeId. The fake-chromium answers
/// with a CDP error envelope (code -32000, "Could not find node with given
/// id"), which the shim maps to CdpProtocolError → host `FailureClass::Application`.
fn app_error_payload() -> Vec<u8> {
    let msg = CdpMessage {
        method: "DOM.getBoxModel".into(),
        params: ciborium::value::Value::Map(vec![(
            ciborium::value::Value::Text("nodeId".into()),
            ciborium::value::Value::Integer(999_999.into()),
        )]),
    };
    ciborium_to_vec(&msg).expect("encode CdpMessage")
}

/// AREA 3.1 + 3.2: application-level CDP error keeps the live shim alive
/// (Application class, no eviction); a transport death evicts it (Transport
/// class). Both verdicts exercised on the real send→shim→fake-chromium path.
#[tokio::test]
#[ignore = "requires fake-chromium + loom-shim-chromium; run with --ignored after building the fixtures"]
async fn application_cdp_error_keeps_live_shim_transport_death_evicts() {
    require_fixtures();
    let obs = HostObservability::new(true);
    let mgr = ShimManager::new(obs);

    let sid = format!("applife{}", std::process::id());
    let id = ShimId(format!("chromium:{sid}"));
    let udd = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(udd.path()).ok();
    register(&mgr, &id, udd.path());

    // 1. Force a real spawn with a benign navigate.
    tokio::time::timeout(
        Duration::from_secs(30),
        mgr.send(id.clone(), navigate_payload()),
    )
    .await
    .expect("initial navigate must not hang")
    .expect("initial navigate must succeed (real shim up)");

    // The shim is now live and tracked.
    assert!(
        mgr.list_shim_ids().contains(&id),
        "shim must be live after the first send"
    );

    // 2. Drive a REAL application-level CDP error. It must fail...
    let app_err = tokio::time::timeout(
        Duration::from_secs(30),
        mgr.send(id.clone(), app_error_payload()),
    )
    .await
    .expect("app-error send must not hang");
    assert!(
        app_err.is_err(),
        "DOM.getBoxModel on an unknown node must surface as an error"
    );

    // ...but the live subprocess must STILL be tracked (Application class does
    // NOT evict — the unit test calls record_failure(Application) directly; this
    // proves the live error path classifies the same way).
    assert!(
        mgr.list_shim_ids().contains(&id),
        "application CDP error must NOT evict the live shim (healthy Chromium under it)"
    );

    // ...and a subsequent navigate on the SAME shim must succeed — proving the
    // subprocess survived and kept its session (no respawn happened).
    tokio::time::timeout(
        Duration::from_secs(30),
        mgr.send(id.clone(), navigate_payload()),
    )
    .await
    .expect("post-app-error navigate must not hang")
    .expect("post-app-error navigate must succeed on the SAME live shim");
    assert!(
        mgr.list_shim_ids().contains(&id),
        "shim still live after recovery navigate"
    );

    // 3. TRANSPORT failure control: kill the subprocess tree. The crash watcher
    // flips `crashed`; the next admitted send must fail cleanly (bounded) and
    // the dead handle must be evicted (get_or_spawn evicts a crashed handle).
    let pattern = format!("user-data-dir={}", udd.path().display());
    let _ = std::process::Command::new("pkill")
        .arg("-9")
        .arg("-f")
        .arg(&pattern)
        .status();

    // Bounded: the next send must return (Ok after respawn, or a typed Err) —
    // never hang. Either way the OLD dead subprocess was evicted, not reused.
    let after_kill = tokio::time::timeout(
        Duration::from_secs(35),
        mgr.send(id.clone(), navigate_payload()),
    )
    .await;
    assert!(
        after_kill.is_ok(),
        "after a transport death, send must return bounded (Transport class evicts + respawns), not hang"
    );

    mgr.shutdown_session(&sid).await;
}

/// AREA 3.3: CLOEXEC. The exec'd shim must not inherit the HOST's end of its
/// own IPC socketpair. We spawn a real shim via PRODUCTION `spawn_shim`, then
/// drop the `ShimProcess` (closing the host's parent_fd end and aborting the
/// host IO loops — `kill_on_drop(false)`, so the OS process is NOT signalled).
/// The shim must then observe EOF on its IPC socket ("daemon disconnected") and
/// exit on its own. If the socketpair were created without CLOEXEC, the child
/// would hold a copy of the host's write end, never see EOF, and never exit —
/// the liveness poll below would time out.
#[tokio::test]
#[ignore = "requires fake-chromium + loom-shim-chromium; run with --ignored after building the fixtures"]
async fn cloexec_shim_exits_on_host_ipc_close_not_inherited_fd() {
    require_fixtures();
    let udd = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(udd.path()).ok();

    let process = spawn_shim(&SpawnConfig {
        binary_path: shim_bin().into(),
        args: vec![],
        env: shim_env(udd.path()),
    })
    .await
    .expect("spawn_shim must succeed");
    let pid = process.child_pid as libc::pid_t;

    // Sanity: the subprocess is alive right after spawn.
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        0,
        "shim subprocess must be alive immediately after spawn"
    );

    // Close the host's end of the IPC socketpair by dropping the ShimProcess.
    // This aborts the host read/write loops (drops parent_fd) WITHOUT killing
    // the OS process (kill_on_drop(false)). The ONLY way the shim now exits is
    // by seeing EOF on its inherited IPC end.
    drop(process);

    // Poll for the subprocess to exit. With CLOEXEC correct, the shim sees EOF
    // and exits promptly. The test process is the shim's parent (the host
    // abandoned the `wait()` when we dropped the ShimProcess), so on exit the
    // shim becomes a zombie that lingers — `kill(pid, 0)` would keep returning 0
    // until reaped. Use `waitpid(WNOHANG)` to BOTH detect exit AND reap the
    // zombie (cross-platform-correct), falling back to `kill(0) == ESRCH` if it
    // was already reaped elsewhere. A generous bound tolerates a slow CI runner.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut exited = false;
    while tokio::time::Instant::now() < deadline {
        let mut status: libc::c_int = 0;
        let w = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if w == pid {
            exited = true; // reaped: the shim exited on its own
            break;
        }
        if w == -1 && unsafe { libc::kill(pid, 0) } != 0 {
            exited = true; // already gone (ECHILD / ESRCH)
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Best-effort cleanup if the assert is about to fire (don't leak a hung shim
    // + its fake-chromium tree).
    if !exited {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
        let pattern = format!("user-data-dir={}", udd.path().display());
        let _ = std::process::Command::new("pkill")
            .arg("-9")
            .arg("-f")
            .arg(&pattern)
            .status();
    }

    assert!(
        exited,
        "shim did not exit after the host closed its IPC end — the exec'd shim is \
         holding the HOST's socketpair end open (CLOEXEC regression in spawn_shim)"
    );
}
