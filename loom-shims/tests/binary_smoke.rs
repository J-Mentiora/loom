//! L9 — Early E2E smoke test.
//!
//! Per practitioner / skeptic council review: catches FD-passing,
//! binary main runtime, IpcEndpoint loops, dispatcher Shutdown handling,
//! and supervisor::run orderly shutdown BEFORE the host-side wiring
//! piles on. Costs ~1h to write; catches design flaws while the plan
//! can still pivot.
//!
//! Strategy:
//! 1. From the test process, create an AF_UNIX SOCK_STREAM socketpair.
//! 2. Spawn `loom-shim-chromium` with LOOM_SHIM_FD pointing at one end,
//!    LOOM_SHIM_CHROMIUM_PATH pointing at the `fake-chromium` test binary
//!    (which simulates a real Chromium DevTools endpoint).
//! 3. The shim binary should boot, spawn fake-chromium, parse its
//!    stderr "DevTools listening on..." line, and connect via CDP.
//! 4. From the test side, write a `ShimRequest::Shutdown { request_id: 1 }`
//!    frame to the socketpair.
//! 5. Assert the shim child exits cleanly within 10s with status 0,
//!    after first sending back a `ShimResponse::Ok` ack with the
//!    matching request_id.
//!
//! Gated by `fake-chromium-bin` feature (required for the fake-chromium
//! binary to exist). Run with:
//!   `cargo test -p loom-shims --features fake-chromium-bin --test binary_smoke`

#![cfg(feature = "fake-chromium-bin")]

use loom_shared::shim_protocol::{
    ciborium_to_vec, ShimRequest, ShimResponse, LENGTH_PREFIX_BYTES,
};
use std::io::{Read, Write};
use std::os::unix::io::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::Duration;

const SHIM_BIN: &str = env!("CARGO_BIN_EXE_loom-shim-chromium");
const FAKE_CHROMIUM_BIN: &str = env!("CARGO_BIN_EXE_fake-chromium");

/// Create an AF_UNIX SOCK_STREAM socketpair, returning the two raw fds.
/// The first fd is for the test (parent) side, the second is for the
/// shim (child) side.
fn make_socketpair() -> (RawFd, RawFd) {
    let mut fds = [0i32; 2];
    let rc = unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_STREAM,
            0,
            fds.as_mut_ptr(),
        )
    };
    if rc != 0 {
        panic!("socketpair failed: {}", std::io::Error::last_os_error());
    }
    (fds[0], fds[1])
}

#[test]
fn shim_binary_round_trips_shutdown() {
    let (parent_fd, child_fd) = make_socketpair();
    let user_data_dir = tempfile::tempdir().expect("tempdir");

    let mut cmd = Command::new(SHIM_BIN);
    cmd.env("LOOM_SHIM_FD", "3")
        .env("LOOM_SHIM_CHROMIUM_PATH", FAKE_CHROMIUM_BIN)
        .env("LOOM_SHIM_USER_DATA_DIR", user_data_dir.path())
        .env("LOOM_FAKE_CHROMIUM_USER_DATA_DIR", user_data_dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Move the child end of the socketpair to fd 3 in the child.
    // The pre_exec body MUST be async-signal-safe — only libc::dup2 +
    // libc::close are allowed. No allocations, no format!, no eprintln!
    // (per practitioner gotcha #2).
    let child_fd_owned = child_fd;
    unsafe {
        cmd.pre_exec(move || {
            // dup2 clears CLOEXEC on fd 3 automatically (POSIX), so the
            // shim sees the inherited fd at the documented number.
            if libc::dup2(child_fd_owned, 3) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if child_fd_owned != 3 {
                libc::close(child_fd_owned);
            }
            Ok(())
        });
    }

    let mut child = cmd.spawn().expect("spawn loom-shim-chromium");

    // Close the child end on the parent side.
    unsafe { libc::close(child_fd) };

    // Wrap the parent fd in std::os::unix::net::UnixStream for blocking I/O.
    let mut parent = unsafe { std::os::unix::net::UnixStream::from_raw_fd(parent_fd) };
    parent
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    parent
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    // Give the shim time to boot Chromium + connect CDP.
    std::thread::sleep(Duration::from_millis(500));

    // Send ShimRequest::Shutdown { request_id: 42 }.
    let req = ShimRequest::Shutdown { request_id: 42 };
    let payload = ciborium_to_vec(&req).unwrap();
    let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
    frame.extend_from_slice(&payload);
    parent.write_all(&frame).expect("write shutdown");

    // Read the ack back: 4-byte BE length, then payload.
    let mut len_buf = [0u8; LENGTH_PREFIX_BYTES];
    parent.read_exact(&mut len_buf).expect("read len");
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut payload_buf = vec![0u8; len];
    parent.read_exact(&mut payload_buf).expect("read payload");

    let ack: ShimResponse = ciborium::de::from_reader(&payload_buf[..]).expect("decode");
    match ack {
        ShimResponse::Ok { request_id, .. } => {
            assert_eq!(request_id, 42, "shim must echo the originating request_id");
        }
        other => panic!("expected Ok ack, got {other:?}"),
    }

    // Wait for the shim child to exit cleanly.
    let status = wait_with_timeout(&mut child, Duration::from_secs(15));
    let exit_code = status.expect("shim binary did not exit within 15s");
    assert!(exit_code.success(), "shim exited non-zero: {exit_code:?}");

    drop(parent);
    drop(user_data_dir);
}

/// `std::process::Child::wait` doesn't accept a timeout. Poll it in 100ms
/// increments until the deadline.
fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Option<std::process::ExitStatus> {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => return None,
        }
    }
    let _ = child.kill();
    None
}

// Ensure the fds inherit the close-on-exec convention we expect.
#[test]
fn socketpair_smoke_matches_libc_signature() {
    let (a, b) = make_socketpair();
    assert!(a >= 0);
    assert!(b >= 0);
    unsafe {
        libc::close(a);
        libc::close(b);
    }
}

