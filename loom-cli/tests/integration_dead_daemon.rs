//! AC4 / R4 — a dead daemon socket yields a typed, actionable error.
//!
//! Locks the stable substrings asserted by the onboarding PRD (FND-0001): an
//! RPC-bearing command run against a dead daemon must print an error containing
//! both `no daemon` and `loom serve`, and must exit 1 (the daemon-class error
//! code). Pinning the substrings here keeps `connection_message` from drifting
//! out from under the onboarding contract.
//!
//! Boundedness note: this task's title says "with RPC timeout". The bound on the
//! dead-daemon path is *structural*, not timed — `auth_manager::read_hello_token`
//! short-circuits to `DaemonNotRunning` via `daemon_alive()` before any socket
//! await, and the hung-but-alive case is bounded by the already-wired
//! `request_timeout`. There is therefore no unbounded await to race, so these
//! tests assert exit code + message only and add NO wall-clock bound (no-flake
//! bar, FND-0006).
//!
//! Two layers, mirroring `integration_daemon_harness.rs`:
//! - **No-spawn**: never-started harness → absent socket + no PID file → runs on
//!   every `cargo test`.
//! - **Lifecycle**: start→stop leaves a stale socket + dead PID; `#[ignore]`d and
//!   run in CI under `--include-ignored`, like the other daemon-backed e2e.

#![cfg(unix)]

mod common;

use common::daemon_test_harness::DaemonTestHarness;

/// Both required substrings, asserted together with a helpful failure dump.
fn assert_dead_daemon_stderr(stderr: &str) {
    assert!(
        stderr.contains("no daemon"),
        "dead-daemon stderr must contain `no daemon` (AC4/FND-0001); got: {stderr:?}"
    );
    assert!(
        stderr.contains("loom serve"),
        "dead-daemon stderr must name the remedy `loom serve` (AC4/FND-0001); got: {stderr:?}"
    );
}

#[test]
fn rpc_command_with_no_daemon_prints_typed_error() {
    // Never started: the harness's unique socket does not exist and the hermetic
    // HOME holds no daemon PID file, so the lookup resolves deterministically to
    // DaemonNotRunning (the hermetic env rules out a stray dev-machine daemon on
    // the default socket path).
    let h = DaemonTestHarness::new();
    assert!(
        !h.socket_path().exists(),
        "precondition: socket must be absent"
    );

    let output = h
        .loom_command()
        .args(["session", "create"])
        .output()
        .expect("spawning the loom CLI must succeed");

    assert_eq!(
        output.status.code(),
        Some(1),
        "a dead daemon is a daemon-class error → exit 1; stderr was: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_dead_daemon_stderr(&String::from_utf8_lossy(&output.stderr));
}

#[test]
#[ignore = "spawns a real loom-daemon; run under `cargo test -- --include-ignored`"]
fn rpc_command_after_daemon_dies_prints_typed_error() {
    // Realistic "the daemon was up, then went away" path: start writes a real PID
    // file + binds the socket; stop SIGKILLs it, leaving a stale socket and a PID
    // pointing at a dead process. The next RPC command must still resolve to the
    // same typed DaemonNotRunning error (and clean up the stale auth files).
    let mut h = DaemonTestHarness::new();
    h.start();
    assert!(
        h.socket_path().exists(),
        "daemon should have bound its socket"
    );
    h.stop();
    assert!(
        !h.is_running(),
        "daemon must be stopped for the dead-socket path"
    );

    let output = h
        .loom_command()
        .args(["session", "create"])
        .output()
        .expect("spawning the loom CLI must succeed");

    assert_eq!(
        output.status.code(),
        Some(1),
        "a dead daemon is a daemon-class error → exit 1; stderr was: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_dead_daemon_stderr(&String::from_utf8_lossy(&output.stderr));
}
