//! End-to-end wire test for the typed session-capacity contract
//! (typed-capacity-errors): a real `loom-daemon` pinned to
//! `LOOM_MAX_CONCURRENT_SESSIONS=2` must reject the create that saturates the
//! cap with the typed `session_cap_exceeded` envelope — `{active, cap, hint}`
//! in `data`, never the opaque `internal_error` — and accept a new create as
//! soon as one session closes.
//!
//! Hermetic: no real Chromium needed. `LOOM_CHROMIUM_PATH` points at any
//! executable (`/bin/sh`) so the daemon's boot-time resolver registers a
//! browser (the `has_chromium` fail-fast in `session_create` passes);
//! `session.create` itself never spawns the browser — that happens lazily on
//! the first web action, which this test never issues.

#![cfg(unix)]

mod common;

use common::daemon_test_harness::DaemonTestHarness;

/// Run `loom --json session create` and return (exit-ok, stdout, stderr).
fn create_session(h: &DaemonTestHarness) -> (bool, String, String) {
    let out = h
        .loom_command()
        .args(["--json", "session", "create"])
        .output()
        .expect("run loom session create");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn session_id_from(stdout: &str) -> String {
    let v: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("session.create stdout must be JSON");
    v.get("session_id")
        .and_then(|s| s.as_str())
        .unwrap_or_else(|| panic!("no session_id in create output: {stdout}"))
        .to_string()
}

#[test]
fn cap_hit_is_typed_on_the_wire_and_recovers_after_close() {
    let mut h = DaemonTestHarness::new()
        .env("LOOM_MAX_CONCURRENT_SESSIONS", "2")
        .env("LOOM_CHROMIUM_PATH", "/bin/sh");
    h.start();

    // Saturate the cap: two creates succeed.
    let (ok1, out1, err1) = create_session(&h);
    assert!(ok1, "first create must succeed; stderr:\n{err1}");
    let sid1 = session_id_from(&out1);
    let (ok2, _out2, err2) = create_session(&h);
    assert!(ok2, "second create must succeed; stderr:\n{err2}");

    // The third create is rejected with the TYPED cap error.
    let (ok3, out3, err3) = create_session(&h);
    let combined = format!("{out3}{err3}");
    assert!(!ok3, "create beyond the cap must fail; got:\n{combined}");
    assert!(
        combined.contains("session_cap_exceeded"),
        "rejection must carry the typed code; got:\n{combined}"
    );
    assert!(
        !combined.contains("internal_error"),
        "cap rejection must NOT collapse to internal_error; got:\n{combined}"
    );
    // Structured {active, cap} details survive to the CLI surface
    // (`data:` line of the receipt error), plus the actionable hint.
    assert!(
        combined.contains("\"active\":2") && combined.contains("\"cap\":2"),
        "rejection must carry active/cap in data; got:\n{combined}"
    );
    assert!(
        combined.contains("loom session reap"),
        "rejection must carry the remediation hint; got:\n{combined}"
    );

    // Close one session → a slot frees → create succeeds again.
    let close = h
        .loom_command()
        .args(["--json", "session", "close", &sid1])
        .output()
        .expect("run loom session close");
    assert!(
        close.status.success(),
        "close must succeed; stderr:\n{}",
        String::from_utf8_lossy(&close.stderr)
    );
    let (ok4, _out4, err4) = create_session(&h);
    assert!(
        ok4,
        "create after freeing a slot must succeed; stderr:\n{err4}"
    );
}
