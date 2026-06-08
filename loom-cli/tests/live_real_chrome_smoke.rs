//! Real-Chrome smoke (settle-capture), NO network: navigate `about:blank` with
//! `until=settled` against a REAL Chromium and assert the receipt is
//! `settled`/`reached` with a real DOM + screenshot. Isolates "does the settle
//! path work on a real browser at all" from the live heavy-SPA + external-
//! network shape (which a network-restricted CI/sandbox can't run). Overridable
//! via `LOOM_SMOKE_URL` for ad-hoc probing against a real site.
//! Gated on LOOM_LIVE_E2E=1 + LOOM_CHROMIUM_PATH. Dumps daemon stderr on exit.

#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use common::daemon_test_harness::DaemonTestHarness;

#[test]
#[ignore = "real Chromium; gated on LOOM_LIVE_E2E=1 + LOOM_CHROMIUM_PATH"]
fn real_chrome_settled_navigate_about_blank() {
    if std::env::var("LOOM_LIVE_E2E").as_deref() != Ok("1") {
        eprintln!("skip: set LOOM_LIVE_E2E=1 + LOOM_CHROMIUM_PATH to run");
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
        // Keep real Chrome off the OS keychain (no "Chrome Safe Storage" prompt)
        // and out of the sandbox so the test is non-interactive.
        .env(
            "LOOM_CHROMIUM_EXTRA_FLAGS",
            "--no-sandbox --disable-dev-shm-usage --use-mock-keychain --password-store=basic",
        )
        .with_ready_timeout(std::time::Duration::from_secs(30));
    provision_web_world(harness.home());
    harness.start();

    let sid = {
        let out = run_loom(&harness, &["session", "create", "--profile", "standard"]);
        eprintln!(
            "session create: status={} stderr={}",
            out.status, out.stderr
        );
        let v: serde_json::Value = serde_json::from_str(&out.stdout)
            .unwrap_or_else(|e| panic!("session create not JSON: {e}; raw={:?}", out.stdout));
        v["session_id"].as_str().unwrap().to_string()
    };

    // Trivial, instant, no-network page (in the URL allowlist). Overridable via
    // LOOM_SMOKE_URL for ad-hoc probing against a real http(s) site.
    let url = std::env::var("LOOM_SMOKE_URL").unwrap_or_else(|_| "about:blank".to_string());
    let url = url.as_str();
    let out = run_loom(
        &harness,
        &[
            "action",
            "web.navigate",
            "--session",
            &sid,
            "--url",
            url,
            "--until",
            "settled",
        ],
    );
    eprintln!(
        "navigate: status={}\nstdout={}\nstderr={}\n--- daemon stderr ---\n{}",
        out.status,
        out.stdout,
        out.stderr,
        std::fs::read_to_string(harness.home().join("daemon.stderr")).unwrap_or_default()
    );

    let receipt: serde_json::Value = serde_json::from_str(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "navigate not JSON: {e}; status={} stderr={:?}",
            out.status, out.stderr
        )
    });
    assert_eq!(
        receipt["status"], "success",
        "real-Chrome settled navigate must succeed; got {receipt}"
    );
    assert_eq!(receipt["settle_until"], "settled");
    assert_eq!(
        receipt["settle_outcome"], "reached",
        "a trivial about:blank page must settle to reached on real Chrome; got {receipt}"
    );

    let _ = run_loom(&harness, &["session", "close", &sid]);
}

// ── helpers (same as live_settle_regression) ──────────────────────────────

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

fn provision_web_world(home: &Path) {
    let cfg_loom = home.join(".config").join("loom");
    let surfaces_dir = cfg_loom.join("surfaces");
    std::fs::create_dir_all(&surfaces_dir).unwrap();
    std::os::unix::fs::symlink(cwasm_path(), surfaces_dir.join("loom_surface_web.cwasm")).unwrap();
    let schemas_dir = cfg_loom.join("schemas").join("v1");
    std::fs::create_dir_all(&schemas_dir).unwrap();
    loom_cli::postinstall_runner::schema_step(&schemas_dir).unwrap();
    let permissive = r#"{"request":{"type":"object","additionalProperties":true},"response":{"type":"object","additionalProperties":true}}"#;
    for m in [
        "session.create",
        "session.close",
        "session.list",
        "session.validate",
    ] {
        std::fs::write(schemas_dir.join(format!("{m}.json")), permissive).unwrap();
    }
}

fn cwasm_path() -> &'static Path {
    static CWASM: OnceLock<PathBuf> = OnceLock::new();
    CWASM.get_or_init(|| {
        let wasm = workspace_root().join("target/wasm32-wasip2/release/loom_surface_web.wasm");
        assert!(
            wasm.exists(),
            "build: cargo build --target wasm32-wasip2 -p loom-surface-web --release"
        );
        let cwasm =
            workspace_root().join("target/loom-real-chrome-smoke-cwasm/loom_surface_web.cwasm");
        std::fs::create_dir_all(cwasm.parent().unwrap()).unwrap();
        if !cwasm.exists() {
            use loom_host::compiler::Compiler;
            use loom_host::wasm_runtime::{WasmRuntime, WasmRuntimeConfig};
            let rt = WasmRuntime::new(WasmRuntimeConfig::default()).unwrap();
            Compiler::new(rt).compile_module(&wasm, &cwasm).unwrap();
        }
        cwasm
    })
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}
