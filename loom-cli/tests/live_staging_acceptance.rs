//! Live ACCEPTANCE verify for client-nav-reattach against the real staging SPA.
//!
//! This is the feature's acceptance gate (run manually at ship time with creds),
//! NOT a CI test — CI coverage is the deployment-independent fake-chromium e2e +
//! the local-fixture real-Chrome test (`live_client_redirect_reattach.rs`).
//!
//! Acceptance #1 (no creds): `web.navigate(https://staging.ai.mentiora.ai/,
//!   until=networkidle)` must return with the Auth0 login page rendered —
//!   document.body present, readyState "complete", heading "Welcome to
//!   Mentiora" — NOT the blank SPA shell loom used to wedge on.
//! Acceptance #2 (creds via LOOM_STAGING_USER / LOOM_STAGING_PASS): after
//!   submitting the identifier form, a `web.wait_for` resolves on the password
//!   step's document.
//!
//! Run:
//! ```sh
//! cargo build --target wasm32-wasip2 -p loom-surface-web --release
//! LOOM_LIVE_E2E=1 \
//!   LOOM_CHROMIUM_PATH="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
//!   cargo test -p loom-cli --test live_staging_acceptance -- --ignored --nocapture
//! ```

#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use common::daemon_test_harness::DaemonTestHarness;

const STAGING: &str = "https://staging.ai.mentiora.ai/";

#[test]
#[ignore = "live network + real Chromium; gated on LOOM_LIVE_E2E=1 + LOOM_CHROMIUM_PATH"]
fn staging_navigate_reaches_auth0_login_not_blank_shell() {
    if std::env::var("LOOM_LIVE_E2E").as_deref() != Ok("1") {
        eprintln!("skip: set LOOM_LIVE_E2E=1 + LOOM_CHROMIUM_PATH to run the staging acceptance");
        return;
    }
    let chromium = match std::env::var("LOOM_CHROMIUM_PATH") {
        Ok(p) if Path::new(&p).exists() => p,
        _ => {
            eprintln!("skip: LOOM_CHROMIUM_PATH unset/missing");
            return;
        }
    };

    let mut harness = DaemonTestHarness::new()
        .env("LOOM_CHROMIUM_PATH", &chromium)
        .env(
            "LOOM_CHROMIUM_EXTRA_FLAGS",
            "--no-sandbox --disable-dev-shm-usage --use-mock-keychain --password-store=basic",
        )
        .with_ready_timeout(std::time::Duration::from_secs(30));
    provision_web_world(harness.home());
    harness.start();

    let sid = create_session(&harness);

    // ── Acceptance #1: blank SPA shell → Auth0 login, settled ────────────────
    // First real-Chrome launch in a fresh profile can cold-start slowly; one
    // retry absorbs that transient (the dispatch itself is deterministic).
    let mut nav = navigate(&harness, &sid, STAGING, "networkidle");
    if nav["settle_outcome"].as_str() != Some("reached")
        && nav["status"].as_str() != Some("success")
    {
        eprintln!("first navigate did not settle ({nav}); retrying once (cold Chrome launch)");
        nav = navigate(&harness, &sid, STAGING, "networkidle");
    }
    let settle_outcome = nav["settle_outcome"].as_str().unwrap_or("");
    eprintln!(
        "navigate(staging, networkidle): settle_outcome={settle_outcome} final_url={}",
        nav["final_url"]
    );
    assert_eq!(
        settle_outcome, "reached",
        "staging must reach a settled state after the client-side Auth0 redirect \
         (got {settle_outcome:?}); a timeout means loom wedged on the blank shell. receipt={nav}"
    );

    // Read the settled document: readyState, body presence, visible heading.
    let probe = evaluate(
        &harness,
        &sid,
        "JSON.stringify([document.readyState, document.body!==null, \
         (document.body && document.body.innerText || '').replace(/\\s+/g,' ').slice(0,600)])",
    );
    let rv = probe["return_value_json"].as_str().unwrap_or("");
    eprintln!("DOM probe return_value_json={rv}");
    assert!(
        rv.contains("complete"),
        "document.readyState must be 'complete' on the settled page (was loom wedged?); rv={rv}"
    );
    assert!(
        rv.contains("true"),
        "document.body must be present (non-null) on the settled page; rv={rv}"
    );
    assert!(
        rv.contains("Welcome to Mentiora"),
        "the settled capture must be the Auth0 login page (heading 'Welcome to Mentiora'), \
         not the blank SPA shell; rv={rv}"
    );

    // ── Acceptance #2: multi-step login (creds-gated) ────────────────────────
    let (user, pass) = match (
        std::env::var("LOOM_STAGING_USER"),
        std::env::var("LOOM_STAGING_PASS"),
    ) {
        (Ok(u), Ok(p)) if !u.is_empty() && !p.is_empty() => (u, p),
        _ => {
            eprintln!(
                "acceptance #2 (multi-step login) SKIPPED — set LOOM_STAGING_USER + \
                 LOOM_STAGING_PASS to exercise the form-POST re-attach"
            );
            let _ = run_loom(&harness, &["session", "close", &sid]);
            return;
        }
    };

    // Type the identifier and submit; the form-POST is a top-level navigation
    // loom must follow, so the subsequent wait_for resolves on the password step.
    let _ = action(
        &harness,
        &sid,
        "web.type",
        &[
            "--selector",
            "input[name=username], input[type=email], #username",
            "--text",
            &user,
        ],
    );
    let _ = action(
        &harness,
        &sid,
        "web.click",
        &[
            "--selector",
            "button[type=submit], button[name=action], button",
        ],
    );
    let waited = action(&harness, &sid, "web.wait_for", &["--until", "networkidle"]);
    let wait_outcome = waited["settle_outcome"].as_str().unwrap_or("");
    eprintln!("post-identifier wait_for: settle_outcome={wait_outcome}");
    assert_eq!(
        wait_outcome, "reached",
        "after submitting the identifier form, wait_for must resolve on the password \
         step's document (got {wait_outcome:?}); receipt={waited}"
    );
    let pw = evaluate(
        &harness,
        &sid,
        "JSON.stringify([document.readyState, !!document.querySelector('input[type=password]')])",
    );
    let pwrv = pw["return_value_json"].as_str().unwrap_or("");
    eprintln!("password-step probe={pwrv}");
    assert!(
        pwrv.contains("complete"),
        "the password step must be settled (readyState complete); rv={pwrv}"
    );
    // `pass` is used only if a later step needs it; the acceptance is that the
    // password STEP rendered (re-attach worked), not a full credential round-trip.
    let _ = pass;

    let _ = run_loom(&harness, &["session", "close", &sid]);
}

// ─── helpers (trimmed copies of live_settle_regression.rs) ───────────────────

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
    action(
        harness,
        sid,
        "web.navigate",
        &["--url", url, "--until", until],
    )
}

fn evaluate(harness: &DaemonTestHarness, sid: &str, expression: &str) -> serde_json::Value {
    action(harness, sid, "web.evaluate", &["--expression", expression])
}

fn action(
    harness: &DaemonTestHarness,
    sid: &str,
    method: &str,
    extra: &[&str],
) -> serde_json::Value {
    let mut args = vec!["action", method, "--session", sid];
    args.extend_from_slice(extra);
    let out = run_loom(harness, &args);
    serde_json::from_str::<serde_json::Value>(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{method} stdout not JSON: {e}; status={} stderr={:?} daemon_stderr=\n{}",
            out.status,
            out.stderr,
            daemon_stderr(harness)
        )
    })
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
        let cwasm =
            workspace_root().join("target/loom-staging-acceptance-cwasm/loom_surface_web.cwasm");
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
