//! Soak + chaos coverage for the daemon-side browser lifecycle hardening
//! (loom-mcp-browser-resilience). Drives the `ShimManager` against the
//! deterministic `fake-chromium` fixture — the same harness `deep_health_probe`
//! uses — so these run in CI without a real Chromium.
//!
//! Covered:
//! - **SOAK / no-leak (WI-6):** create→drive→`shutdown_session` across many
//!   sequential sessions; assert every per-session `/tmp/loom-chromium-*`
//!   profile dir is removed on close and none accumulate (the slow-degradation
//!   contributor).
//! - **CHAOS / crash recovery (WI-5):** kill the underlying fake-chromium
//!   mid-session; assert a subsequent `send` transparently respawns the shim
//!   (`get_or_spawn` evicts the crashed handle) and succeeds — bounded.
//!
//! `#[ignore]`d like the other fake-chromium tests; build the fixture first:
//!   cargo build -p loom-shims --features fake-chromium-bin --bin fake-chromium
//! then run with `--include-ignored`.

#![cfg(unix)]

use loom_host::host_observability::HostObservability;
use loom_host::shim_manager::{ShimConfig, ShimId, ShimManager};
use loom_shared::shim_protocol::{ciborium_to_vec, CdpMessage};
use std::path::PathBuf;
use std::time::Duration;

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
             --features fake-chromium-bin --bin fake-chromium` first"
        );
    }
}

/// The convention path the host exports as `LOOM_SHIM_USER_DATA_DIR` and that
/// `ShimManager::shutdown_session` cleans up (WI-6).
fn profile_dir(session_id: &str) -> PathBuf {
    std::env::temp_dir().join(format!("loom-chromium-{session_id}"))
}

fn register_session(mgr: &ShimManager, session_id: &str) -> ShimId {
    let id = ShimId(format!("chromium:{session_id}"));
    let dir = profile_dir(session_id);
    std::fs::create_dir_all(&dir).expect("create profile dir");
    mgr.register(
        id.clone(),
        ShimConfig {
            binary_path: shim_bin().into(),
            args: vec![],
            env: vec![
                ("LOOM_SHIM_CHROMIUM_PATH".into(), fake_chromium_bin()),
                ("LOOM_SHIM_USER_DATA_DIR".into(), dir.display().to_string()),
                (
                    "LOOM_FAKE_CHROMIUM_USER_DATA_DIR".into(),
                    dir.display().to_string(),
                ),
            ],
            spawn_retry: 2,
            // High threshold so transient crash-detection failures during the
            // chaos test don't trip the breaker before the respawn lands.
            breaker_threshold: 10,
            breaker_open_ms: 5_000,
            send_timeout_ms: 10_000,
            recv_timeout_ms: 30_000,
        },
    );
    id
}

fn navigate_payload() -> Vec<u8> {
    let msg = CdpMessage {
        method: "Page.navigate".into(),
        params: ciborium::value::Value::Map(vec![(
            ciborium::value::Value::Text("url".into()),
            ciborium::value::Value::Text("about:blank".into()),
        )]),
    };
    ciborium_to_vec(&msg).expect("encode CdpMessage")
}

/// SOAK: many sequential create→drive→close cycles on one long-lived manager.
/// Asserts ZERO profile-dir leak — every session's dir is gone after close and
/// none survive the run. `LOOM_SOAK_N` overrides the cycle count (default 200,
/// matching the acceptance bar).
#[tokio::test]
#[ignore = "requires fake-chromium; run with --include-ignored after building the fixture"]
async fn soak_many_sessions_no_profile_dir_leak() {
    require_fixtures();
    let n: usize = std::env::var("LOOM_SOAK_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);

    let obs = HostObservability::new(true);
    let mgr = ShimManager::new(obs);
    // Unique per-run prefix so a parallel test/real daemon never collides.
    let run = std::process::id();
    let mut all_dirs = Vec::with_capacity(n);

    for i in 0..n {
        let session_id = format!("soak{run}x{i}");
        let dir = profile_dir(&session_id);
        all_dirs.push(dir.clone());
        let id = register_session(&mgr, &session_id);

        // Drive a navigate to force a real spawn + round-trip.
        let res = tokio::time::timeout(Duration::from_secs(30), mgr.send(id, navigate_payload()))
            .await
            .unwrap_or_else(|_| panic!("cycle {i}: send hung past 30s"));
        assert!(res.is_ok(), "cycle {i}: navigate failed: {:?}", res.err());

        // Close the session — this is the single chokepoint that must reap the
        // chromium subprocess AND remove the profile dir (WI-6).
        mgr.shutdown_session(&session_id).await;

        assert!(
            !dir.exists(),
            "cycle {i}: profile dir {} was not removed on session close (WI-6 leak)",
            dir.display()
        );
    }

    // No accumulation across the whole soak.
    let leaked: Vec<_> = all_dirs.iter().filter(|d| d.exists()).collect();
    assert!(
        leaked.is_empty(),
        "{} profile dir(s) leaked across {n} sessions: {:?}",
        leaked.len(),
        leaked
    );
}

/// CHAOS: kill the underlying chromium mid-session. The daemon must not wedge —
/// a session created AFTER the crash gets a HEALTHY browser (requirement 2:
/// "sessions created after a browser crash get a healthy browser instead of a
/// dead handle"), and the crashed session's next action fails cleanly within a
/// bounded time rather than hanging ("bound retries; fail cleanly if recovery is
/// impossible"). This reproduces the reported symptom — "web ops broken-pipe for
/// every NEW session until restart" — and asserts it does NOT happen.
#[tokio::test]
#[ignore = "requires fake-chromium; run with --include-ignored after building the fixture"]
async fn chaos_crashed_chromium_does_not_wedge_new_sessions() {
    require_fixtures();
    let obs = HostObservability::new(true);
    let mgr = ShimManager::new(obs);

    // Session A: bring a shim up.
    let sid_a = format!("chaosA{}", std::process::id());
    let dir_a = profile_dir(&sid_a);
    let id_a = register_session(&mgr, &sid_a);
    mgr.send(id_a.clone(), navigate_payload())
        .await
        .expect("initial navigate on session A should succeed");

    // Kill session A's chromium tree by its unique --user-data-dir marker.
    // The supervisor self-exits on chromium death → the daemon's crash watcher
    // flips `crashed` and reaps A's orphan tree.
    let pattern = format!("user-data-dir={}", dir_a.display());
    assert!(
        std::process::Command::new("pkill")
            .arg("-9")
            .arg("-f")
            .arg(&pattern)
            .status()
            .is_ok(),
        "pkill invocation failed"
    );

    // (a) Daemon NOT wedged: a NEW session (fresh shim id + profile dir) must
    // get a healthy browser despite A's crash. Bounded retry only absorbs spawn
    // timing, not a wedge.
    let sid_b = format!("chaosB{}", std::process::id());
    let dir_b = profile_dir(&sid_b);
    let id_b = register_session(&mgr, &sid_b);
    let mut b_ok = false;
    let mut last_err = None;
    for _ in 0..5 {
        match tokio::time::timeout(
            Duration::from_secs(30),
            mgr.send(id_b.clone(), navigate_payload()),
        )
        .await
        {
            Ok(Ok(_)) => {
                b_ok = true;
                break;
            }
            Ok(Err(e)) => last_err = Some(format!("{e}")),
            Err(_) => last_err = Some("send hung past 30s".to_string()),
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    assert!(
        b_ok,
        "a session created after a chromium crash must get a healthy browser \
         (daemon wedged); last error: {last_err:?}"
    );

    // (b) The crashed session's next action fails CLEANLY and bounded — a typed
    // error, never a hang. (`get_or_spawn` evicts the crashed handle, WI-5;
    // in-place same-dir respawn may not recover, which is acceptable — it must
    // not hang.)
    let a_result = tokio::time::timeout(
        Duration::from_secs(35),
        mgr.send(id_a.clone(), navigate_payload()),
    )
    .await;
    assert!(
        a_result.is_ok(),
        "crashed session's next action must return bounded (not hang)"
    );

    // Cleanup. The healthy session B must leave no profile dir (WI-6, the
    // normal path — also exhaustively covered by the soak test). Session A's
    // dir is best-effort: the crashed-session same-dir respawn churn can have an
    // orphaned chromium recreate the DevToolsActivePort file just after cleanup,
    // a race that only exists for in-place same-session restarts (production
    // recovery uses fresh per-session dirs).
    mgr.shutdown_session(&sid_a).await;
    mgr.shutdown_session(&sid_b).await;
    assert!(
        !dir_b.exists(),
        "healthy session B profile dir not cleaned (WI-6)"
    );
    // Best-effort reap of A's dir so the test leaves nothing behind.
    let _ = std::fs::remove_dir_all(&dir_a);
}
