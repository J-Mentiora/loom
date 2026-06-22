//! Tier-2 (real Chromium, DEPLOYMENT-INDEPENDENT) coverage for
//! client-nav-reattach: after a loaded page begins a top-level navigation it
//! initiated ITSELF (`window.location` assign / `<meta refresh>` /
//! `<form method=post>` submit), loom must re-attach to the new document and
//! settle on it — not wedge on the blank in-flight shell.
//!
//! This is the real-browser sibling of the hermetic fake-chromium e2e
//! (`loom-host/tests/integration_navigate_settle_e2e.rs`). Unlike
//! `live_settle_regression.rs` it does NOT hit a live deployment
//! (dev.ai.mentiora.ai) — it serves the redirect fixtures from a LOCAL HTTP
//! server bound to 127.0.0.1, so it is "real CI" with zero external dependency
//! (the user requirement: a real test "not tied to a specific deployment").
//!
//! ## Double-gated — never runs in normal CI
//! - `#[ignore]`, so a plain `cargo test` skips it.
//! - Even under `--ignored` it early-returns UNLESS `LOOM_LIVE_E2E=1`, so the
//!   broad fake-chromium ignored sweeps don't launch real Chrome.
//!
//! ## Running it
//! ```sh
//! cargo build --target wasm32-wasip2 -p loom-surface-web --release
//! LOOM_LIVE_E2E=1 \
//!   LOOM_CHROMIUM_PATH=/Applications/Google\ Chrome.app/Contents/MacOS/Google\ Chrome \
//!   cargo test -p loom-cli --test live_client_redirect_reattach -- --ignored --nocapture
//! ```
//! No network required — only a real Chrome and the local fixture server.

#![cfg(unix)]

mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use common::daemon_test_harness::DaemonTestHarness;

/// The final landing page every redirect fixture ends on.
const FINAL_HTML: &str = "<!doctype html><html><head><title>Welcome</title></head>\
     <body><h1 id=\"welcome\">Welcome</h1><p>final page</p></body></html>";

/// A local HTTP server that routes the three client-side top-level redirect
/// shapes to a shared `/final` page. Loops until the test drops it (the daemon
/// makes several requests per navigate: shell, redirect target, favicon).
fn fixture_html(path: &str, xfinal: &str) -> Option<String> {
    let body = match path {
        // (1) window.location assignment — the canonical SPA→IdP redirect.
        "/loc" => "<!doctype html><html><head><title>shell</title></head><body>\
             <script>window.location.href='/final';</script></body></html>"
            .to_string(),
        // (2) <meta http-equiv=refresh> — a top-level navigation with no script.
        "/meta" => "<!doctype html><html><head><title>shell</title>\
             <meta http-equiv=\"refresh\" content=\"0; url=/final\"></head>\
             <body>redirecting…</body></html>"
            .to_string(),
        // (3) auto-submitting form POST — a form-submission navigation.
        "/form" => "<!doctype html><html><head><title>shell</title></head><body>\
             <form id=\"f\" method=\"post\" action=\"/final\"></form>\
             <script>document.getElementById('f').submit();</script></body></html>"
            .to_string(),
        // (4) CROSS-ORIGIN window.location — redirects to a DIFFERENT origin
        // (the real SPA→Auth0 shape, which destroys the execution context).
        // `xfinal` is the other server's absolute /final URL.
        "/xloc" => format!(
            "<!doctype html><html><head><title>shell</title></head><body>\
             <script>window.location.href='{xfinal}';</script></body></html>"
        ),
        "/final" => FINAL_HTML.to_string(),
        _ => return None,
    };
    Some(body)
}

struct FixtureServer {
    addr: std::net::SocketAddr,
}

/// Spawn a fixture server. `xfinal` is the absolute `/final` URL the `/xloc`
/// fixture redirects to (used to model a cross-origin redirect to a second
/// server); pass `""` for a server that only ever redirects same-origin.
fn spawn_fixture_server(xfinal: String) -> FixtureServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture server");
    let addr = listener.local_addr().expect("local_addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            // Request line: "<METHOD> <path> HTTP/1.1". A form-POST lands on
            // /final via POST — serve the same body regardless of method.
            let path = req
                .split_whitespace()
                .nth(1)
                .map(|p| p.split('?').next().unwrap_or(p).to_string())
                .unwrap_or_default();
            let resp = match fixture_html(&path, &xfinal) {
                Some(body) => format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                ),
                None => {
                    let body = "not found";
                    format!(
                        "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                }
            };
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    FixtureServer { addr }
}

#[test]
#[ignore = "real Chromium (no network); gated on LOOM_LIVE_E2E=1 + LOOM_CHROMIUM_PATH"]
fn client_redirect_reattach_against_local_fixtures() {
    if std::env::var("LOOM_LIVE_E2E").as_deref() != Ok("1") {
        eprintln!(
            "skip: real-Chrome reattach test is opt-in — set LOOM_LIVE_E2E=1 (and \
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

    // Origin B serves the cross-origin landing page; origin A serves the shells
    // (and a /xloc that redirects to B/final — a genuine cross-origin top-level
    // navigation, which swaps the renderer's execution context like SPA→Auth0).
    let origin_b = spawn_fixture_server(String::new());
    let xfinal = format!("http://{}/final", origin_b.addr);
    let server = spawn_fixture_server(xfinal);
    let base = format!("http://{}", server.addr);

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

    // Each fixture begins a DIFFERENT kind of unsolicited top-level navigation
    // to /final. A wedged loom (pre-fix) lands on the blank shell: settle
    // times out and final_url stays the shell URL. The fix must reach a settled
    // verdict on the redirected /final document.
    for (label, path) in [
        ("window.location", "/loc"),
        ("meta-refresh", "/meta"),
        ("form-POST", "/form"),
        ("cross-origin window.location", "/xloc"),
    ] {
        let url = format!("{base}{path}");
        let receipt = navigate(&harness, &sid, &url, "settled");
        let settle_outcome = receipt["settle_outcome"].as_str().unwrap_or("");
        let final_url = receipt["final_url"].as_str().unwrap_or("");

        assert_eq!(
            receipt["settle_until"], "settled",
            "[{label}] settled navigate must report settle_until=settled; got {receipt}"
        );
        assert_eq!(
            settle_outcome, "reached",
            "[{label}] loom must re-attach to the client-redirected document and reach a \
             settled state (got settle_outcome={settle_outcome:?}) — a timeout means it \
             wedged on the blank in-flight shell; receipt={receipt}"
        );
        assert!(
            final_url.ends_with("/final"),
            "[{label}] final_url must reflect the re-attached document (…/final), got \
             {final_url:?}; loom is still attached to the shell"
        );
        eprintln!("[{label}] reattach OK → final_url={final_url}");
    }

    let _ = run_loom(&harness, &["session", "close", &sid]);
}

// ─── CLI helpers (trimmed copies of live_settle_regression.rs) ───────────────

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

// ─── Hermetic web-world provisioning (same paths as live_settle_regression) ──

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
        let cwasm = workspace_root().join("target/loom-reattach-cwasm/loom_surface_web.cwasm");
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
