//! Self-tests for the shared [`DaemonTestHarness`] fixture (task 01, AC11).
//!
//! Two layers:
//! - **No-spawn** tests assert the harness's isolation guarantees (unique
//!   socket, hermetic HOME) without a built daemon — they run on every
//!   `cargo test`.
//! - **Lifecycle** tests spawn a real `loom-daemon`; like the other
//!   daemon-backed e2e tests they are `#[ignore]`d and run in CI under
//!   `--include-ignored`. They use 0 retries and a single bounded ready-wait
//!   (FND-0006 no-flake bar).

#![cfg(unix)]

mod common;

use common::daemon_test_harness::DaemonTestHarness;

#[test]
fn each_harness_gets_a_unique_socket() {
    let a = DaemonTestHarness::new();
    let b = DaemonTestHarness::new();
    assert_ne!(
        a.socket_path(),
        b.socket_path(),
        "every harness must own a distinct socket so daemon tests can run in parallel"
    );
}

#[test]
fn socket_lives_under_hermetic_home_and_is_unbound_before_start() {
    let h = DaemonTestHarness::new();
    assert!(
        h.socket_path().is_absolute(),
        "socket path must be absolute"
    );
    assert!(
        h.socket_path().starts_with(h.home()),
        "socket must live inside the harness's private tempdir"
    );
    assert!(
        h.home().is_dir(),
        "hermetic HOME (tempdir) must exist for the daemon to write into"
    );
    assert!(
        !h.socket_path().exists(),
        "socket must not exist until the daemon binds it in start()"
    );
    assert!(
        !h.is_running(),
        "no daemon should be running before start()"
    );
    assert!(h.hello_token().is_none(), "no HELLO token before start()");
}

#[test]
#[ignore = "spawns a real loom-daemon; run under `cargo test -- --include-ignored`"]
fn start_blocks_until_ready_then_socket_is_connectable() {
    use std::os::unix::net::UnixStream;

    let mut h = DaemonTestHarness::new();
    h.start();

    assert!(h.is_running(), "daemon should be running after start()");
    assert!(
        h.hello_token().is_some_and(|t| !t.is_empty()),
        "start() must capture a non-empty HELLO token"
    );
    assert!(
        h.socket_path().exists(),
        "ready-wait must not return before the daemon has bound the socket"
    );
    // A successful connect proves the listener is genuinely accepting — i.e. the
    // ready-wait didn't race ahead of the socket going live.
    UnixStream::connect(h.socket_path()).expect("daemon socket must be connectable after start()");

    h.stop();
    assert!(!h.is_running(), "daemon should be stopped after stop()");
    assert!(h.hello_token().is_none(), "token cleared on stop()");
}

#[test]
#[ignore = "spawns two real loom-daemons; run under `cargo test -- --include-ignored`"]
fn two_harnesses_run_concurrently_without_collision() {
    let mut a = DaemonTestHarness::new();
    let mut b = DaemonTestHarness::new();

    a.start();
    b.start();

    assert_ne!(
        a.socket_path(),
        b.socket_path(),
        "concurrent harnesses must not share a socket"
    );
    assert!(
        a.socket_path().exists() && b.socket_path().exists(),
        "both daemons should have bound their own sockets"
    );
    assert!(
        a.hello_token().is_some() && b.hello_token().is_some(),
        "both daemons should have announced readiness independently"
    );
    // RAII `Drop` stops both daemons and removes both tempdirs.
}
