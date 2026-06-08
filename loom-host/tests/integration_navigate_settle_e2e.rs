//! End-to-end proof that the settle-capture readiness gate delivers the
//! brief's reproduction shapes through the REAL shim path (host →
//! `loom-shim-chromium` → `fake-chromium`), not just at the pure
//! state-machine layer.
//!
//! The 16 `ReadinessMachine` unit tests (`loom-shims/src/readiness_monitor/
//! interface_tests.rs`) already prove the verdict logic for every branch from
//! a scripted observation feed. THESE tests prove the WIRING: that a scripted
//! page actually drives the live `SettleDriver` → `ReadinessMachine` →
//! `NavigateOutcome` chain to the same verdict. Each case scripts the page via
//! `LOOM_FAKE_CHROMIUM_SCRIPT` (a per-tick settle-probe feed + optional
//! perpetual in-flight requests; see `fake-chromium.rs`):
//!
//!   (a) Client-side redirect SPA: the probe reports the shell URL, then the
//!       final URL, then quiesces. Assert the capture is gated until AFTER the
//!       redirect (`reached`, having run the full quiet window) — the old
//!       commit-time capture would have grabbed the blank shell.
//!   (c) Async-after-load: `readyState==complete` from tick 1, but a delayed
//!       DOM-mutation burst arrives later. Assert `settled` waits for it
//!       (`reached`, strictly longer than a clean page's minimum settle) — a
//!       naive readyState-only wait would have captured too early.
//!   (d1) Never-settles (network): perpetual in-flight requests above the idle
//!       threshold. Assert the bounded tick-ceiling returns a typed `timeout`
//!       receipt and does NOT hang.
//!   (d2) Never-settles (DOM): the DOM mutates every tick forever (network
//!       quiet, document complete). Assert a typed `dom_unstable` receipt,
//!       distinct from the network timeout.
//!
//! No wall-clock dependence: every verdict is a pure function of the scripted
//! per-tick observation sequence (DET-CORE). `settle_ms` is `ticks * pacing`,
//! a deterministic integer, so the `>=` bounds below are stable.
//!
//! Run:
//!   cargo build -p loom-shims --features fake-chromium-bin --bin fake-chromium
//!   cargo build -p loom-cli --bin loom-shim-chromium
//!   cargo test -p loom-host --test integration_navigate_settle_e2e -- --ignored
//!
//! Marked `#[ignore]` so a default `cargo test --workspace` doesn't force the
//! fake-chromium build.

#![cfg(unix)]

use loom_host::host_observability::HostObservability;
use loom_host::shim_manager::{ShimConfig, ShimId, ShimManager};
use std::time::Duration;

fn target_bin_dir() -> std::path::PathBuf {
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

fn assert_binaries_built() {
    let fake_path = fake_chromium_bin();
    let shim_path = shim_bin();
    if !std::path::Path::new(&fake_path).exists() {
        panic!(
            "fake-chromium binary not built at {fake_path}; run `cargo build -p loom-shims --features fake-chromium-bin --bin fake-chromium` first"
        );
    }
    if !std::path::Path::new(&shim_path).exists() {
        panic!(
            "loom-shim-chromium binary not built at {shim_path}; run `cargo build -p loom-cli --bin loom-shim-chromium` first"
        );
    }
}

/// Build a ShimManager whose `fake-chromium` is driven by `script_json`
/// (written into the per-session user-data dir and pointed at via
/// `LOOM_FAKE_CHROMIUM_SCRIPT`). The returned `TempDir` owns both the
/// user-data dir and the script file; keep it alive for the test's duration.
fn make_manager_with_script(
    session_label: &str,
    script_json: &str,
) -> (std::sync::Arc<ShimManager>, ShimId, tempfile::TempDir) {
    let user_data_dir = tempfile::tempdir().expect("tempdir");
    let script_path = user_data_dir.path().join("settle_script.json");
    std::fs::write(&script_path, script_json).expect("write settle script");

    let obs = HostObservability::new(true);
    let mgr = ShimManager::new(obs);
    let id = ShimId(format!("chromium:{session_label}"));
    mgr.register(
        id.clone(),
        ShimConfig {
            binary_path: shim_bin().into(),
            args: vec![],
            env: vec![
                ("LOOM_SHIM_CHROMIUM_PATH".into(), fake_chromium_bin()),
                (
                    "LOOM_SHIM_USER_DATA_DIR".into(),
                    user_data_dir.path().display().to_string(),
                ),
                (
                    "LOOM_FAKE_CHROMIUM_USER_DATA_DIR".into(),
                    user_data_dir.path().display().to_string(),
                ),
                (
                    "LOOM_FAKE_CHROMIUM_SCRIPT".into(),
                    script_path.display().to_string(),
                ),
            ],
            spawn_retry: 1,
            breaker_threshold: 3,
            breaker_open_ms: 5_000,
            send_timeout_ms: 30_000,
            recv_timeout_ms: 60_000,
        },
    );
    (mgr, id, user_data_dir)
}

/// Drive one `settled` navigate against the scripted fake page.
async fn navigate_settled(
    mgr: &std::sync::Arc<ShimManager>,
    id: &ShimId,
    outer_timeout: Duration,
) -> loom_shared::navigate_outcome::NavigateOutcome {
    tokio::time::timeout(
        outer_timeout,
        mgr.send_navigate(
            id.clone(),
            "test-action".to_string(),
            0,
            0,
            "http://fake.test/status/200".into(),
            30_000,
            loom_shared::types::Seed(0),
            loom_shared::types::EpochMs(0),
            true,
            "settled".to_string(),
            true,
        ),
    )
    .await
    .expect("send_navigate timed out (a never-settles case must return a TYPED receipt, not hang)")
    .expect("send_navigate returned an error")
}

// ── (a) Client-side redirect SPA: settle waits past the blank shell ──────────
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn redirect_spa_settles_on_final_page_not_shell() {
    assert_binaries_built();
    // tick1: shell, not yet complete. tick2: shell complete (a commit-time
    // capture would grab THIS — the blank shell). tick3: client-side redirect
    // to the final URL. tick4+: final URL, quiet → eventually settles.
    let script = r#"{
        "settle_probe": [
            [false, "http://fake.test/shell", 0],
            [true,  "http://fake.test/shell", 2],
            [true,  "http://fake.test/final", 0],
            [true,  "http://fake.test/final", 0]
        ]
    }"#;
    let (mgr, id, _udd) = make_manager_with_script("settle-redirect", script);

    let outcome = navigate_settled(&mgr, &id, Duration::from_secs(30)).await;

    assert_eq!(
        outcome.settle_outcome, "reached",
        "the redirect quiesces, so settled must be reached"
    );
    assert_eq!(outcome.settle_until, "settled");
    // The gate ran the full quiet window AFTER the shell→final redirect; a
    // commit-time capture (settle_ms == 0) would have grabbed the blank shell.
    assert!(
        outcome.settle_ms >= 25,
        "settled must wait through the redirect + quiet window, got settle_ms={}",
        outcome.settle_ms
    );

    mgr.shutdown_session("settle-redirect").await;
}

// ── (c) Async content injected AFTER readyState=complete is awaited ──────────
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn async_content_after_load_is_awaited() {
    assert_binaries_built();
    // readyState==complete from tick 1 (a naive readyState-only wait stops
    // here), but a delayed fetch injects DOM mutations at ticks 3–4 before the
    // page finally quiesces. `settled` must wait for the late content.
    let script = r#"{
        "settle_probe": [
            [true, "http://fake.test/app", 0],
            [true, "http://fake.test/app", 0],
            [true, "http://fake.test/app", 5],
            [true, "http://fake.test/app", 5],
            [true, "http://fake.test/app", 0]
        ]
    }"#;
    let (mgr, id, _udd) = make_manager_with_script("settle-async", script);

    let outcome = navigate_settled(&mgr, &id, Duration::from_secs(30)).await;

    assert_eq!(
        outcome.settle_outcome, "reached",
        "the async content eventually quiesces, so settled must be reached"
    );
    // A clean page settles in exactly the quiet window (settle_ms == 25). The
    // late mutation burst resets the DOM-quiet run, so this MUST take strictly
    // longer — proof the gate awaited content a readyState-only wait would miss.
    assert!(
        outcome.settle_ms > 25,
        "settled must wait past the async DOM burst (strictly longer than a clean \
         page's minimum), got settle_ms={}",
        outcome.settle_ms
    );

    mgr.shutdown_session("settle-async").await;
}

// ── (d1) Never-settles (network): bounded timeout, typed receipt, no hang ────
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn never_settles_network_returns_bounded_timeout() {
    assert_binaries_built();
    // The page is complete and the DOM is quiet, but 3 requests (> the idle
    // threshold of 2) stay in flight forever — a persistent connection /
    // perpetual poll. `networkidle` can never be met → the tick ceiling must
    // return a typed `timeout` (NOT `dom_unstable`, NOT a hang).
    let script = r#"{
        "settle_probe": [[true, "http://fake.test/poll", 0]],
        "perpetual_inflight": 3
    }"#;
    let (mgr, id, _udd) = make_manager_with_script("settle-net-timeout", script);

    // Bounded by the shim's DEFAULT_NAVIGATE_BUDGET (10s) → 2000-tick ceiling.
    // The VERDICT is deterministic (tick-count based), but the wall-clock to
    // walk the ceiling = ceiling × (tick pacing + per-tick CDP round-trip),
    // which on a slow/loaded CI runner can approach ~30s — so this outer guard
    // (which only exists to fail loudly on a TRUE infinite hang) needs ample
    // headroom over that. 180s is ~5× the observed CI wall-time.
    let outcome = navigate_settled(&mgr, &id, Duration::from_secs(180)).await;

    assert_eq!(
        outcome.settle_outcome, "timeout",
        "persistent in-flight requests must hit the bounded network timeout"
    );
    assert!(
        outcome.network_count_at_settle >= 3,
        "the timeout receipt must report the stuck in-flight count, got {}",
        outcome.network_count_at_settle
    );

    mgr.shutdown_session("settle-net-timeout").await;
}

// ── (d2) Never-settles (DOM): typed dom_unstable, distinct from timeout ──────
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn never_settles_dom_returns_dom_unstable() {
    assert_binaries_built();
    // Network quiet, document complete, URL stable — but the DOM mutates every
    // single tick forever (a perpetual animation / re-render). Everything
    // EXCEPT the DOM quiesces, so the ceiling must return `dom_unstable`,
    // distinct from the network `timeout` above.
    let script = r#"{
        "settle_probe": [[true, "http://fake.test/anim", 4]]
    }"#;
    let (mgr, id, _udd) = make_manager_with_script("settle-dom-unstable", script);

    // 180s outer guard: same 2000-tick ceiling wall-time rationale as the
    // never-settles-network case (the verdict is deterministic; only the
    // wall-clock to reach the ceiling varies with CI runner speed).
    let outcome = navigate_settled(&mgr, &id, Duration::from_secs(180)).await;

    assert_eq!(
        outcome.settle_outcome, "dom_unstable",
        "a perpetually-mutating DOM (everything else quiet) must report dom_unstable"
    );

    mgr.shutdown_session("settle-dom-unstable").await;
}
