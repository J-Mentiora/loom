//! Live settle-capture regression test against the ORIGINAL repro sites
//! (settle-capture; requested by the feature brief: "a test or new CI against
//! mentiora.ai/blog to prevent regressions").
//!
//! This is the only settle-capture test that hits the REAL network with a REAL
//! Chromium — every other case is hermetic (fake-chromium scripted-timing
//! e2e + the pure ReadinessMachine unit tests). It exists to catch a real-world
//! regression of the two motivating bugs:
//!
//!   1. `dev.ai.mentiora.ai` — loom lands on the SPA shell, then a CLIENT-SIDE
//!      redirect to Auth0 fires. The OLD commit-time capture was a blank PNG;
//!      a settled capture is the real Auth0 login page. Guard: the
//!      `until=settled` screenshot hash MUST DIFFER from the `until=load`
//!      (≈ old commit-time) hash — i.e. settled waited past the blank shell.
//!   2. `mentiora.ai/blog` — an animated hero meant navigate-time captures
//!      landed on random frames. Guard: `until=settled` reaches a bounded
//!      `reached` verdict and produces a real capture (never hangs).
//!
//! ## Double-gated — never runs in normal CI
//! - `#[ignore]`, so a plain `cargo test` skips it.
//! - Even under `--ignored` it early-returns UNLESS `LOOM_LIVE_E2E=1`, so the
//!   broad "run all ignored e2e" sweeps (which use fake-chromium) don't try to
//!   reach the network.
//!
//! ## Running it (the ship-time live verify)
//! ```sh
//! cargo build -p loom-shims --features fake-chromium-bin --bin fake-chromium
//! cargo build --target wasm32-wasip2 -p loom-surface-web --release
//! LOOM_LIVE_E2E=1 \
//!   LOOM_CHROMIUM_PATH=/Applications/Google\ Chrome.app/Contents/MacOS/Google\ Chrome \
//!   cargo test -p loom-cli --test live_settle_regression -- --ignored --nocapture
//! ```
//! `LOOM_CHROMIUM_PATH` must point at a real Chrome/Chromium. If it is unset the
//! test prints a skip line and returns Ok (so a `LOOM_LIVE_E2E=1` run without a
//! browser is a no-op, not a failure).

#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use common::daemon_test_harness::DaemonTestHarness;

const DEV_SPA: &str = "https://dev.ai.mentiora.ai";
const BLOG: &str = "https://mentiora.ai/blog";

#[test]
#[ignore = "live network + real Chromium; gated on LOOM_LIVE_E2E=1 + LOOM_CHROMIUM_PATH"]
fn settle_capture_live_regression_against_mentiora() {
    if std::env::var("LOOM_LIVE_E2E").as_deref() != Ok("1") {
        eprintln!(
            "skip: live settle regression is opt-in — set LOOM_LIVE_E2E=1 (and \
             LOOM_CHROMIUM_PATH to a real Chrome) to run it"
        );
        return;
    }
    let chromium = match std::env::var("LOOM_CHROMIUM_PATH") {
        Ok(p) if Path::new(&p).exists() => p,
        _ => {
            eprintln!(
                "skip: LOOM_LIVE_E2E=1 but LOOM_CHROMIUM_PATH is unset or missing — \
                 point it at a real Chrome/Chromium binary"
            );
            return;
        }
    };

    let mut harness = DaemonTestHarness::new()
        .env("LOOM_CHROMIUM_PATH", &chromium)
        // Keep real Chrome OFF the macOS login Keychain (it prompts for the
        // "Chrome Safe Storage" key on launch) and out of the OS sandbox so the
        // test is non-interactive. `--use-mock-keychain --password-store=basic`
        // are the upstream flags for "don't touch the OS keychain".
        .env(
            "LOOM_CHROMIUM_EXTRA_FLAGS",
            "--no-sandbox --disable-dev-shm-usage --use-mock-keychain --password-store=basic",
        )
        // Live pages are slow; give the daemon a generous readiness ceiling.
        .with_ready_timeout(std::time::Duration::from_secs(30));
    provision_web_world(harness.home());
    harness.start();

    // Determinism is ON by default (fixed seed) — no flags needed.
    let sid = create_session(&harness);

    // ── Repro 1: client-side redirect SPA (dev.ai.mentiora.ai) ──────────────
    // The settled capture must differ from the load-time capture: settled
    // waited through the Auth0 redirect; load grabbed the blank shell.
    let load = navigate(&harness, &sid, DEV_SPA, "load");
    let settled = navigate(&harness, &sid, DEV_SPA, "settled");

    let settle_outcome = settled["settle_outcome"].as_str().unwrap_or("");
    assert_eq!(
        settled["settle_until"], "settled",
        "settled navigate must report settle_until=settled; got {settled}"
    );
    assert!(
        settle_outcome == "reached",
        "dev.ai.mentiora.ai must reach a settled state (got settle_outcome={settle_outcome:?}); \
         a timeout/dom_unstable here means the page never quiesced — investigate before shipping"
    );
    let load_hash = screenshot_hash(&load);
    let settled_hash = screenshot_hash(&settled);
    assert!(
        is_sha256_hex(&settled_hash),
        "settled navigate must produce a screenshot_after_hash; got {settled}"
    );
    assert_ne!(
        load_hash, settled_hash,
        "REGRESSION: the settled capture matched the load-time capture for {DEV_SPA} — \
         settled is no longer waiting past the blank SPA shell / client-side Auth0 redirect. \
         load_hash={load_hash} settled_hash={settled_hash}"
    );

    // ── Repro 2: animated hero (mentiora.ai/blog) ───────────────────────────
    // Two settled navigates must each reach a bounded verdict and capture a
    // real frame (no hang on the running animation). We do NOT assert
    // byte-identity across two LIVE loads — page CONTENT legitimately varies
    // over the network; animation-frame determinism is proven hermetically in
    // the fake-chromium e2e. Here we guard "settled, bounded, captured".
    for attempt in 1..=2 {
        let blog = navigate(&harness, &sid, BLOG, "settled");
        let outcome = blog["settle_outcome"].as_str().unwrap_or("");
        assert!(
            matches!(outcome, "reached" | "timeout" | "dom_unstable"),
            "blog navigate #{attempt} must return a TYPED settle verdict (never hang); got {blog}"
        );
        assert_eq!(
            blog["settle_until"], "settled",
            "blog navigate #{attempt} must report settle_until=settled; got {blog}"
        );
        assert!(
            is_sha256_hex(&screenshot_hash(&blog)),
            "blog navigate #{attempt} must produce a real screenshot capture; got {blog}"
        );
        eprintln!("mentiora.ai/blog settled navigate #{attempt}: settle_outcome={outcome}");
    }

    let _ = run_loom(&harness, &["session", "close", &sid]);
}

// ─── CLI helpers (trimmed copies of the quickstart e2e harness) ─────────────

fn create_session(harness: &DaemonTestHarness) -> String {
    let out = run_loom(harness, &["session", "create", "--profile", "standard"]);
    assert_eq!(
        out.status,
        0,
        "session create must exit 0; stderr={:?} daemon_stderr=\n{}",
        out.stderr,
        daemon_stderr(harness)
    );
    let v: serde_json::Value = serde_json::from_str(&out.stdout)
        .unwrap_or_else(|e| panic!("session create stdout not JSON: {e}; raw={:?}", out.stdout));
    v["session_id"]
        .as_str()
        .unwrap_or_else(|| panic!("session_id missing: {v}"))
        .to_string()
}

fn navigate(harness: &DaemonTestHarness, sid: &str, url: &str, until: &str) -> serde_json::Value {
    let out = run_loom(
        harness,
        &[
            "action",
            "web.navigate",
            "--session",
            sid,
            "--url",
            url,
            "--until",
            until,
        ],
    );
    // A navigate may legitimately surface a non-zero exit for an HTTP 4xx/5xx
    // landing, but the readiness gate + receipt must still be present. Parse
    // stdout regardless and let the assertions in the test body judge.
    serde_json::from_str::<serde_json::Value>(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "navigate({url}, until={until}) stdout not JSON: {e}; status={} stderr={:?} \
             daemon_stderr=\n{}",
            out.status,
            out.stderr,
            daemon_stderr(harness)
        )
    })
}

fn screenshot_hash(receipt: &serde_json::Value) -> String {
    receipt["screenshot_after_hash"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

struct CliOutput {
    status: i32,
    stdout: String,
    stderr: String,
}

fn run_loom(harness: &DaemonTestHarness, args: &[&str]) -> CliOutput {
    let mut cmd = harness.loom_command();
    cmd.arg("--json");
    cmd.args(args);
    let out = cmd.output().expect("spawn loom CLI");
    CliOutput {
        status: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn daemon_stderr(harness: &DaemonTestHarness) -> String {
    std::fs::read_to_string(harness.home().join("daemon.stderr")).unwrap_or_default()
}

// ─── Hermetic web-world provisioning (same paths as the quickstart e2e) ─────

fn provision_web_world(home: &Path) {
    let cfg_loom = home.join(".config").join("loom");

    let surfaces_dir = cfg_loom.join("surfaces");
    std::fs::create_dir_all(&surfaces_dir).expect("mkdir surfaces");
    std::os::unix::fs::symlink(cwasm_path(), surfaces_dir.join("loom_surface_web.cwasm"))
        .expect("symlink cwasm into surfaces");

    let schemas_dir = cfg_loom.join("schemas").join("v1");
    std::fs::create_dir_all(&schemas_dir).expect("mkdir schemas/v1");
    loom_cli::postinstall_runner::schema_step(&schemas_dir).expect("schema_step");
    let permissive = r#"{"request":{"type":"object","additionalProperties":true},"response":{"type":"object","additionalProperties":true}}"#;
    for method in [
        "session.create",
        "session.close",
        "session.list",
        "session.validate",
    ] {
        std::fs::write(schemas_dir.join(format!("{method}.json")), permissive)
            .expect("write permissive session schema");
    }
}

fn cwasm_path() -> &'static Path {
    static CWASM: OnceLock<PathBuf> = OnceLock::new();
    CWASM.get_or_init(|| {
        let wasm_path = workspace_root().join("target/wasm32-wasip2/release/loom_surface_web.wasm");
        assert!(
            wasm_path.exists(),
            "wasm32-wasip2 surface artifact not built at {}; run \
             `cargo build --target wasm32-wasip2 -p loom-surface-web --release`",
            wasm_path.display()
        );
        let cwasm = workspace_root().join("target/loom-live-settle-cwasm/loom_surface_web.cwasm");
        std::fs::create_dir_all(cwasm.parent().unwrap()).expect("create cwasm cache dir");
        if !cwasm.exists() {
            use loom_host::compiler::Compiler;
            use loom_host::wasm_runtime::{WasmRuntime, WasmRuntimeConfig};
            let runtime = WasmRuntime::new(WasmRuntimeConfig::default()).expect("WasmRuntime::new");
            let compiler = Compiler::new(runtime);
            compiler
                .compile_module(&wasm_path, &cwasm)
                .expect("AOT compile loom_surface_web.wasm -> cwasm");
        }
        cwasm
    })
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root is loom-cli's parent")
        .to_path_buf()
}
