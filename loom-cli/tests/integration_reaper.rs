//! Integration tests for the session + Chromium reaper, driven through a real daemon via
//! `loom session reap`. These exercise the orphan-Chromium GC acceptance criteria end-to-end:
//! reap kills an orphan browser tree and removes its dir, never touches a non-loom dir, and is
//! idempotent. The idle/zombie session paths are covered by the pure-decision unit tests
//! (`loom-daemon/tests/reaper_decide.rs`) — they need a live navigated browser, which the
//! hermetic harness can't spin up without real Chromium.
//!
//! Hermetic: the daemon's `$TMPDIR` is pinned into the harness's private home so the scan only
//! ever sees dirs this test created.

mod common;

use common::daemon_test_harness::DaemonTestHarness;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Spawn a long-lived stand-in for a Chromium tree, in its OWN process group (so the reaper's
/// `killpg(pid)` reaps just it), with the user-data-dir embedded in its command line so the
/// reaper's recycle-guard (`pid_cmdline_contains`) accepts it. Returns the child + its pid.
fn spawn_fake_browser(udd: &Path) -> (std::process::Child, i32) {
    // The leading `: "..."` keeps `sh` from exec-optimising into bare `sleep` (which would
    // drop the user-data-dir from the command line). The pid IS the pgid via process_group(0).
    let script = format!(": \"user-data-dir={}\"; sleep 120", udd.display());
    let child = Command::new("sh")
        .arg("-c")
        .arg(script)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("spawn fake browser");
    let pid = child.id() as i32;
    (child, pid)
}

fn pid_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

fn run_reap(h: &DaemonTestHarness, apply: bool) -> String {
    let mut cmd = h.loom_command();
    cmd.args(["session", "reap"]);
    if apply {
        cmd.arg("--apply");
    }
    let out = cmd.output().expect("run loom session reap");
    assert!(
        out.status.success(),
        "reap exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn reap_kills_orphan_browser_removes_dir_and_is_idempotent() {
    // Pin TMPDIR into the hermetic home; long sweep so the periodic task can't race the
    // explicit reap; zero orphan grace so the freshly-made dir classifies immediately; idle
    // eviction off so it doesn't add noise.
    let h0 = DaemonTestHarness::new();
    let tmp: PathBuf = h0.home().join("reaper-tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    let mut h = h0
        .env("TMPDIR", &tmp)
        .env("LOOM_REAPER_SWEEP_SECS", "300")
        .env("LOOM_REAP_ORPHAN_MIN_AGE_SECS", "0")
        .env("LOOM_REAP_KILL_GRACE_MS", "1500")
        .env("LOOM_SESSION_IDLE_TTL_SECS", "0");
    h.start();

    // An orphan: a loom-chromium dir, a live "browser", and a pidfile pointing at it. No live
    // session owns this id, so it must be reaped.
    let orphan = tmp.join("loom-chromium-orphan0001");
    std::fs::create_dir_all(&orphan).unwrap();
    let (mut child, pid) = spawn_fake_browser(&orphan);
    std::fs::write(orphan.join("loom-chromium.pid"), pid.to_string()).unwrap();
    assert!(pid_alive(pid), "fake browser should be alive after spawn");

    // A NON-loom dir the reaper must never touch.
    let foreign = tmp.join("not-loom-data");
    std::fs::create_dir_all(&foreign).unwrap();

    // `loom doctor` surfaces the orphan count via daemon.health (P10). Doctor exits non-zero
    // in the hermetic env (no real Chromium), so capture stdout regardless of status.
    let doctor = h.loom_command().arg("doctor").output().expect("run doctor");
    let doctor_out = format!(
        "{}{}",
        String::from_utf8_lossy(&doctor.stdout),
        String::from_utf8_lossy(&doctor.stderr)
    );
    assert!(
        doctor_out.contains("orphan_browser_trees=1"),
        "doctor must report the live orphan tree; got:\n{doctor_out}"
    );

    // Dry-run: previews the orphan, changes nothing.
    let preview = run_reap(&h, false);
    assert!(
        preview.contains("1 orphan browser tree"),
        "dry-run should preview the orphan; got:\n{preview}"
    );
    assert!(orphan.is_dir(), "dry-run must NOT remove the orphan dir");
    assert!(pid_alive(pid), "dry-run must NOT kill the orphan process");

    // Apply: kills the tree + removes the dir.
    let applied = run_reap(&h, true);
    assert!(
        applied.contains("1 orphan browser tree"),
        "apply should report the orphan; got:\n{applied}"
    );
    // Give the SIGTERM→SIGKILL grace a moment to land, then reap the zombie.
    std::thread::sleep(Duration::from_millis(2000));
    let _ = child.wait();
    assert!(
        !pid_alive(pid),
        "apply must kill the orphan browser process"
    );
    assert!(
        !orphan.exists(),
        "apply must remove the orphan user-data-dir"
    );

    // The foreign dir is untouched.
    assert!(foreign.is_dir(), "reaper must never touch a non-loom dir");

    // Idempotent: a second apply finds nothing.
    let again = run_reap(&h, true);
    assert!(
        again.contains("0 orphan browser tree"),
        "second apply should be a no-op; got:\n{again}"
    );
}
