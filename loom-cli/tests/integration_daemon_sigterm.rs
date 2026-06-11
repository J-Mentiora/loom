//! Full-process SIGTERM graceful-shutdown coverage for the real `loom-daemon`
//! binary.
//!
//! The existing daemon shutdown tests in `loom-daemon/src/lib.rs`
//! (`shutdown_signal_resolves_on_sigterm` / `_sigint`) are IN-PROCESS: they
//! poll the `shutdown_signal()` future and raise a signal at the test process.
//! That proves the future resolves, but NOT that the daemon binary, end to end,
//! (a) installs the handler on the real serve loop, (b) exits cleanly, and
//! (c) reaps its auth artefacts (`hello.token`, `daemon.pid`) on the way out.
//!
//! This test spawns the REAL `loom-daemon` (via the shared `DaemonTestHarness`,
//! which resolves `CARGO_BIN_EXE_loom-daemon` so Cargo builds it), waits for
//! `HELLO_TOKEN`, sends a real SIGTERM, and asserts the full graceful contract.
//!
//! Failure mode if the graceful path were un-wired: if the daemon ignored
//! SIGTERM, `sigterm_and_wait` would force-kill after the bound and return Err
//! (test fails). If `run()` skipped the `remove_file(token/pid)` cleanup
//! (lib.rs step 11) on shutdown, the artefact-removal assertions would fail. If
//! the serve loop never released the listener, the post-shutdown
//! not-connectable assertion would fail.
//!
//! `#[ignore]`d like the other daemon-spawning lifecycle tests; CI runs it
//! under `--include-ignored`.

#![cfg(unix)]

mod common;

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use common::daemon_test_harness::DaemonTestHarness;

/// Where the daemon writes its auth artefacts, given the data_root we pin via
/// `LOOM_DATA_ROOT`. Mirrors lib.rs step 7 (`<data_root>/auth/...`).
fn auth_paths(data_root: &std::path::Path) -> (PathBuf, PathBuf) {
    let auth = data_root.join("auth");
    (auth.join("hello.token"), auth.join("daemon.pid"))
}

#[test]
#[ignore = "spawns a real loom-daemon; run under `cargo test -- --include-ignored`"]
fn sigterm_exits_cleanly_and_reaps_auth_artefacts() {
    // Pin an explicit data_root so the auth-file location is platform-stable
    // (the default derives from dirs::data_dir(), which differs macOS vs Linux).
    // It still lives under the harness's hermetic HOME tempdir.
    let (mut h, data_root) = DaemonTestHarness::new().with_data_root_under_home();
    h.start();

    let (token_path, pid_path) = auth_paths(&data_root);

    // Preconditions: a healthy, fully-started daemon.
    assert!(h.is_running(), "daemon should be running after start()");
    assert!(
        h.socket_path().exists(),
        "socket must be bound after ready-wait"
    );
    assert!(
        token_path.exists(),
        "hello.token must be written on startup at {}",
        token_path.display()
    );
    assert!(
        pid_path.exists(),
        "daemon.pid must be written on startup at {}",
        pid_path.display()
    );
    // The pid file content must match the live child pid.
    let recorded_pid: u32 = std::fs::read_to_string(&pid_path)
        .expect("read daemon.pid")
        .trim()
        .parse()
        .expect("daemon.pid must contain a numeric pid");
    assert_eq!(
        Some(recorded_pid),
        h.pid(),
        "daemon.pid must record the live daemon's pid"
    );
    // Socket genuinely accepting before we shut down.
    UnixStream::connect(h.socket_path()).expect("socket connectable before SIGTERM");

    // Send the real SIGTERM and wait for a graceful exit.
    let status = h
        .sigterm_and_wait(Duration::from_secs(15))
        .expect("daemon must exit gracefully within 15s of SIGTERM, not be force-killed");

    // "0-ish / graceful": the daemon caught SIGTERM, returned Ok(()) from run(),
    // and the process exited with code 0 — NOT killed by the default SIGTERM
    // disposition (which would show signal=15, code=None) and NOT a panic/abort.
    assert!(
        status.success(),
        "graceful SIGTERM must exit 0 (caught + drained), got: {status:?}"
    );

    // Cleanup contract (lib.rs step 11): auth artefacts removed on shutdown.
    assert!(
        !token_path.exists(),
        "hello.token must be removed on graceful shutdown, still present at {}",
        token_path.display()
    );
    assert!(
        !pid_path.exists(),
        "daemon.pid must be removed on graceful shutdown, still present at {}",
        pid_path.display()
    );

    // The listener FD is released: the socket is no longer connectable. (The
    // socket *inode* may linger on disk — the daemon relies on stale-socket
    // unlink-and-rebind recovery at next start — so we assert non-connectability,
    // not file absence.)
    let connect = UnixStream::connect(h.socket_path());
    assert!(
        connect.is_err(),
        "socket must no longer accept connections after graceful shutdown"
    );
}

/// A second SIGTERM control: SIGINT (interactive Ctrl-C) takes the same
/// graceful path. Proves the daemon's `select!` over {ctrl_c, sigterm} is wired
/// for both, end to end — not just SIGTERM.
#[test]
#[ignore = "spawns a real loom-daemon; run under `cargo test -- --include-ignored`"]
fn sigint_also_exits_cleanly() {
    let (mut h, data_root) = DaemonTestHarness::new().with_data_root_under_home();
    h.start();

    let (token_path, _pid_path) = auth_paths(&data_root);
    assert!(token_path.exists(), "hello.token written on startup");

    // The daemon prints HELLO_TOKEN (step 8) BEFORE it enters the serve loop
    // (step 10), and tokio's SIGINT (`ctrl_c`) handler is installed lazily — on
    // the first poll of the shutdown future inside that loop, completing
    // asynchronously on the signal-driver thread. A SIGINT that lands in the
    // window between HELLO and that registration hits the default disposition
    // and kills the process (signal 2) — a tokio registration race, NOT a
    // daemon wiring defect (the in-process `shutdown_signal_resolves_on_sigint`
    // unit test waits 100ms for the same reason). Gate on a live listener and a
    // settle so the handler is fully installed before we raise SIGINT.
    UnixStream::connect(h.socket_path()).expect("socket connectable (serve loop live)");
    std::thread::sleep(Duration::from_millis(400));

    // Raise SIGINT, then reuse the bounded waiter to drain. (The waiter also
    // sends SIGTERM, but the daemon is already shutting down from SIGINT;
    // whichever lands, the exit must be clean.)
    let pid = h.pid().expect("daemon pid") as libc::pid_t;
    unsafe {
        libc::kill(pid, libc::SIGINT);
    }
    // Drain via the harness's graceful waiter — it sends SIGTERM too, but the
    // daemon will already be shutting down from SIGINT; whichever lands first,
    // the exit must be clean.
    let status = h
        .sigterm_and_wait(Duration::from_secs(15))
        .expect("daemon must exit gracefully after SIGINT");
    assert!(
        status.success(),
        "graceful SIGINT must exit 0, got: {status:?}"
    );
    assert!(
        !token_path.exists(),
        "hello.token must be reaped on graceful shutdown"
    );
}
