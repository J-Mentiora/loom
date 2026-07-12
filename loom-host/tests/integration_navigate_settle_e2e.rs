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
//!   (e) Static page under determinism: the inject-time virtual-time pin
//!       defers the load event until the budget arm (mirroring real headless
//!       Chromium). Assert `reached` well inside the budget — the
//!       settle-timeout-on-static regression pin.
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
        mgr.send_navigate(loom_host::shim_manager::SendNavigateParams {
            id: id.clone(),
            action_id: "test-action".to_string(),
            session_id: 0,
            target_id: 0,
            url: "http://fake.test/status/200".into(),
            budget_ms: 30_000,
            seed: loom_shared::types::Seed(0),
            epoch_ms: loom_shared::types::EpochMs(0),
            blocklist_enabled: true,
            until: "settled".to_string(),
            determinism_enabled: true,
            audio_enabled: false,
        }),
    )
    .await
    .expect("send_navigate timed out (a never-settles case must return a TYPED receipt, not hang)")
    .expect("send_navigate returned an error")
}

/// Drive one `settled` navigate with an explicit `determinism_enabled` flag.
/// (The `navigate_settled` wrapper above is the determinism-ON case; the
/// navigate-degradation regression needs the `--no-determinism` path.)
async fn navigate_with_determinism(
    mgr: &std::sync::Arc<ShimManager>,
    id: &ShimId,
    outer_timeout: Duration,
    determinism_enabled: bool,
) -> loom_shared::navigate_outcome::NavigateOutcome {
    tokio::time::timeout(
        outer_timeout,
        mgr.send_navigate(loom_host::shim_manager::SendNavigateParams {
            id: id.clone(),
            action_id: "test-action".to_string(),
            session_id: 0,
            target_id: 0,
            url: "http://fake.test/status/200".into(),
            budget_ms: 30_000,
            seed: loom_shared::types::Seed(0),
            epoch_ms: loom_shared::types::EpochMs(0),
            blocklist_enabled: true,
            until: "settled".to_string(),
            determinism_enabled,
            audio_enabled: false,
        }),
    )
    .await
    .expect("send_navigate timed out (the degraded session must not hang the whole test)")
    .expect("send_navigate returned an error")
}

/// Drive one `settled` wait_for against the scripted fake page (idempotent
/// spawn — no prior navigate needed).
async fn wait_for_settled(
    mgr: &std::sync::Arc<ShimManager>,
    id: &ShimId,
    outer_timeout: Duration,
) -> loom_shared::navigate_outcome::WaitOutcome {
    tokio::time::timeout(
        outer_timeout,
        mgr.send_wait_for(loom_host::shim_manager::SendWaitForParams {
            id: id.clone(),
            action_id: "test-wait".to_string(),
            session_id: 0,
            target_id: 0,
            until: "settled".to_string(),
            budget_ms: 30_000,
            seed: loom_shared::types::Seed(0),
            epoch_ms: loom_shared::types::EpochMs(0),
            determinism_enabled: true,
            audio_enabled: false,
        }),
    )
    .await
    .expect("send_wait_for timed out (a never-settles case must return a TYPED receipt, not hang)")
    .expect("send_wait_for returned an error")
}

// ── (client-nav-reattach) navigate re-attaches after a client-side top-level ──
// navigation (window.location / <meta refresh> / form-POST). The loaded shell
// completes, then begins a self-initiated navigation whose new document is held
// `readyState:"loading"` until loom RE-ARMS the virtual-time budget. Today loom
// arms the budget once-per-navigate and never re-arms, so it wedges on the blank
// in-flight document and the settle gate reports `timeout`. After the fix it must
// detect the renavigation, re-arm, and settle on the FINAL document (`reached`).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn navigate_reattaches_after_client_redirect() {
    assert_binaries_built();
    // idx0: shell loading. idx1: shell complete (a wedged loom would capture
    // THIS blank shell). idx2: the page client-redirects to the IdP — a NEW
    // top-level document whose load is GATED on a second budget arm
    // (`renavigate_at:[2]`); it stays "loading" until loom re-arms. idx3+: IdP
    // complete + quiet → settles, but ONLY if loom re-attached.
    let script = r#"{
        "settle_probe": [
            [false, "http://fake.test/shell", 0],
            [true,  "http://fake.test/shell", 2],
            [false, "http://fake.test/idp",   0],
            [true,  "http://fake.test/idp",   0],
            [true,  "http://fake.test/idp",   0]
        ],
        "renavigate_at": [2]
    }"#;
    let (mgr, id, _udd) = make_manager_with_script("reattach-nav", script);

    let outcome = navigate_settled(&mgr, &id, Duration::from_secs(30)).await;

    // RED today: loom never re-arms the second document's budget, so the probe
    // stays "loading" and the gate returns `timeout` on the blank IdP shell.
    // GREEN after the fix: loom re-attaches + re-settles on the final page.
    assert_eq!(
        outcome.settle_outcome, "reached",
        "loom must re-attach to the client-redirected document and settle on it, \
         not wedge on the blank in-flight shell (got settle_outcome={}, settle_ms={})",
        outcome.settle_outcome, outcome.settle_ms
    );
    assert_eq!(outcome.settle_until, "settled");

    mgr.shutdown_session("reattach-nav").await;
}

// ── (client-nav-reattach) wait_for resolves on the renavigated document ───────
// Acceptance #2: after a form-POST submit leaves the page on a blank in-flight
// document, a `web.wait_for` must re-attach + resolve on the new (password-step)
// document — NOT hang on the detached one. wait_for never arms virtual time
// today, so only a fresh navigate recovers; the fix gives wait_for the same
// bounded re-arm path.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn wait_for_resolves_on_renavigated_document() {
    assert_binaries_built();
    // idx0: the page is mid-navigation (post form-POST), held "loading" until a
    // budget re-arm (`renavigate_at:[0]`). idx1+: password step complete + quiet.
    let script = r#"{
        "settle_probe": [
            [false, "http://fake.test/password", 0],
            [true,  "http://fake.test/password", 0],
            [true,  "http://fake.test/password", 0]
        ],
        "renavigate_at": [0]
    }"#;
    let (mgr, id, _udd) = make_manager_with_script("reattach-wait", script);

    let outcome = wait_for_settled(&mgr, &id, Duration::from_secs(30)).await;

    assert_eq!(
        outcome.settle_outcome, "reached",
        "wait_for must re-attach + re-arm so it resolves on the new document, \
         not hang on the wedged in-flight one (got settle_outcome={}, settle_ms={})",
        outcome.settle_outcome, outcome.settle_ms
    );
    assert_eq!(outcome.settle_until, "settled");

    mgr.shutdown_session("reattach-wait").await;
}

// ── (client-nav-reattach) replay-equality across the re-attach path ───────────
// FND-0019: re-attaching changes WHICH document is captured (final, not blank
// shell), but the whole shell→redirect→final sequence runs under the paused
// virtual clock, so two same-seed records must produce byte-identical settle
// outcomes + DOM hash. `renavigated` is shim-internal and never enters the hash.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn reattach_redirect_replays_equal() {
    assert_binaries_built();
    let script = r#"{
        "settle_probe": [
            [false, "http://fake.test/shell", 0],
            [true,  "http://fake.test/shell", 2],
            [false, "http://fake.test/idp",   0],
            [true,  "http://fake.test/idp",   0],
            [true,  "http://fake.test/idp",   0]
        ],
        "renavigate_at": [2]
    }"#;

    let (mgr1, id1, _udd1) = make_manager_with_script("reattach-det-1", script);
    let a = navigate_settled(&mgr1, &id1, Duration::from_secs(30)).await;
    mgr1.shutdown_session("reattach-det-1").await;

    let (mgr2, id2, _udd2) = make_manager_with_script("reattach-det-2", script);
    let b = navigate_settled(&mgr2, &id2, Duration::from_secs(30)).await;
    mgr2.shutdown_session("reattach-det-2").await;

    assert_eq!(a.settle_outcome, "reached");
    assert_eq!(
        a.settle_outcome, b.settle_outcome,
        "settle_outcome must replay-equal"
    );
    assert_eq!(
        a.settle_until, b.settle_until,
        "settle_until must replay-equal"
    );
    assert_eq!(
        a.dom_after_sha256, b.dom_after_sha256,
        "same-seed re-attach must capture a byte-identical final DOM (NFR-DET-01)"
    );
}

// ── (client-nav-reattach) redirect chain is bounded by MAX_REATTACH_HOPS ───────
// FND-0016: a page that bounces between login states forever must NOT hang — the
// re-attach loop caps the followed chain (10) and returns the typed `timeout`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn reattach_redirect_chain_is_bounded() {
    assert_binaries_built();
    // 12 redirect hops (> the cap of 10): each hop completes then immediately
    // renavigates. probe idx 2,4,..,24 are the renavigation points; the odd
    // indices between them report the just-loaded (complete) interstitial.
    let mut probe = String::from(
        "[false, \"http://fake.test/shell\", 0],\n            [true, \"http://fake.test/shell\", 2]",
    );
    let mut renav = Vec::new();
    for k in 1..=12 {
        let renav_idx = 2 * k; // even indices: a new redirect begins (loading)
        renav.push(renav_idx.to_string());
        probe.push_str(&format!(
            ",\n            [false, \"http://fake.test/loop{k}\", 0]"
        ));
        probe.push_str(&format!(
            ",\n            [true, \"http://fake.test/loop{k}\", 0]"
        ));
    }
    let script = format!(
        "{{\n            \"settle_probe\": [\n            {probe}\n            ],\n            \"renavigate_at\": [{}]\n        }}",
        renav.join(", ")
    );
    let (mgr, id, _udd) = make_manager_with_script("reattach-bounded", &script);

    // Outer timeout well above the per-action budget: the loop must self-bound on
    // the hop cap, NOT by hanging until this outer guard.
    let outcome = navigate_settled(&mgr, &id, Duration::from_secs(60)).await;

    assert_eq!(
        outcome.settle_outcome, "timeout",
        "an unbounded redirect loop must return the typed timeout (bounded by \
         MAX_REATTACH_HOPS), got settle_outcome={}",
        outcome.settle_outcome
    );

    mgr.shutdown_session("reattach-bounded").await;
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

// ── (e) Static page under determinism: settled is REACHED, well in budget ────
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn static_page_under_determinism_reaches_settled() {
    assert_binaries_built();
    // A trivially static page: ready immediately, URL stable, zero mutations.
    // Under determinism the inject-time virtual-time clock pin DEFERS
    // `Page.loadEventFired` until the navigate arms the per-navigation budget
    // (fake-chromium mirrors real headless Chromium here). The v0.10.1
    // executor awaited load BEFORE arming the budget, so `load_fired` stayed
    // false, the settled latch (which requires it) could never close, and
    // every navigate burned its full wall-clock budget before reporting
    // `settle_outcome="timeout"` on a fully-loaded page
    // (settle-timeout-on-static). This pins the fix.
    let script = r#"{
        "settle_probe": [[true, "http://fake.test/static", 0]]
    }"#;
    let (mgr, id, _udd) = make_manager_with_script("settle-static", script);

    let started = std::time::Instant::now();
    let outcome = navigate_settled(&mgr, &id, Duration::from_secs(30)).await;
    let elapsed = started.elapsed();

    assert_eq!(
        outcome.settle_outcome, "reached",
        "a loaded, request-quiet, mutation-quiet page must reach settled"
    );
    assert_eq!(outcome.settle_until, "settled");
    // A clean page settles in exactly the quiet window (5 ticks → 25ms of
    // virtual settle time); generous headroom, but far below the ceiling.
    assert!(
        outcome.settle_ms <= 100,
        "a clean page must settle in ~the quiet window, got settle_ms={}",
        outcome.settle_ms
    );
    // Well inside the 10s default navigate budget: the OLD order spent the
    // full budget waiting for a load event that cannot fire while the clock
    // pin holds, then walked the settle ceiling on top.
    assert!(
        elapsed < Duration::from_secs(8),
        "static-page navigate must complete well inside the budget, took {elapsed:?}"
    );

    mgr.shutdown_session("settle-static").await;
}

// ── (navigate-degradation) repeated navigate in ONE session does NOT degrade ──
// Regression for the "+20s per web.navigate, then the session wedges" bug. Under
// `--no-determinism` (`determinism_enabled=false`) loom still arms + drains a
// per-navigation virtual-time budget (animation-capture runs regardless of
// determinism). Real headless Chromium — and now fake-chromium — leaves virtual
// time PAUSED once a budget drains. The unfixed exit guard (`!budget_drained`)
// skipped the renderer RESUME after a clean drain, so the clock stayed paused and
// the NEXT navigate's `Page.loadEventFired` was deferred; the determinism-OFF
// navigate path awaits load BEFORE re-arming, so every navigate after the first
// burned its full ~10s load-wait budget, compounding until the 30s host deadline
// wedged the session. The resume-guard fix (`should_resume_virtual_clock`) un-pauses
// the clock on navigate exit whenever determinism is OFF, so every navigate starts
// on an advancing clock and settles promptly. The session (hence the paused-clock
// state) persists across the loop — exactly the multi-navigate journey that broke.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires fake-chromium binary; see file header for build commands"]
async fn repeated_navigate_no_determinism_does_not_degrade() {
    assert_binaries_built();
    let script = r#"{ "settle_probe": [[true, "http://fake.test/static", 0]] }"#;
    let (mgr, id, _udd) = make_manager_with_script("nav-degradation", script);

    // Drive several navigates IN ONE SESSION under --no-determinism. The first is
    // always fast; navigates 2..N are where the clock-left-paused regression bit
    // (each burned the full load-wait budget on an unfixed build).
    for i in 1..=4 {
        let started = std::time::Instant::now();
        let outcome = navigate_with_determinism(&mgr, &id, Duration::from_secs(30), false).await;
        let elapsed = started.elapsed();

        assert_eq!(
            outcome.settle_outcome, "reached",
            "navigate #{i} must settle (got settle_outcome={}, settle_ms={})",
            outcome.settle_outcome, outcome.settle_ms
        );
        // THE REGRESSION ASSERTION: a degraded navigate spent its full ~10s
        // load-wait budget waiting for a load event deferred under the paused
        // clock. The fix keeps every navigate well inside the budget (sub-second).
        // 8s cleanly separates fixed (<1s) from the ~10s-per-call degradation —
        // the same wall-clock bound the determinism static-page test uses.
        assert!(
            elapsed < Duration::from_secs(8),
            "navigate #{i} under --no-determinism must complete promptly with no \
             per-call budget burn; took {elapsed:?} — the virtual clock was left \
             paused and the load event deferred (navigate-degradation)",
        );
    }

    mgr.shutdown_session("nav-degradation").await;
}
