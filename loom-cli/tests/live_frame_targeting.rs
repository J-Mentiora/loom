//! Real-Chrome verification of cross-origin iframe interaction (P0).
//!
//! Reproduces the Mentiora-class blocker with ZERO external dependency: a parent
//! page on `127.0.0.1:A` embeds an `<iframe src="http://127.0.0.1:B/widget">`.
//! Different port ⇒ cross-ORIGIN (so `iframe.contentDocument` is `null` to page
//! JS — the exact blocker the user hit), same host ⇒ same SITE ⇒ same renderer
//! process (in-process frame, reachable via `DOM.describeNode → contentDocument`).
//!
//! The widget's `#b1` click handler SYNCHRONOUSLY renames `#b2` → `#b2done`. So:
//!   - bare `css=#b1`                 → selector_not_found (scope fence: a bare
//!                                       locator never reaches into the iframe)
//!   - `frame=#w >> css=#b1`          → success (cross-origin element resolved +
//!                                       trusted click dispatched at its box)
//!   - `frame=#w >> css=#b2done`      → success ⇒ PROVES the #b1 click actually
//!                                       LANDED and ran its handler, mutating the
//!                                       cross-origin DOM (synchronous — no
//!                                       virtual-time re-arm needed)
//!   - `frame=#w >> css=#b2`          → selector_not_found (it was renamed)
//!
//! ## Double-gated — never runs in normal CI
//! - `#[ignore]`; even under `--ignored` early-returns unless `LOOM_LIVE_E2E=1`.
//!
//! ## Running it
//! ```sh
//! cargo build --target wasm32-wasip2 -p loom-surface-web --release
//! LOOM_LIVE_E2E=1 \
//!   LOOM_CHROMIUM_PATH=/Applications/Google\ Chrome.app/Contents/MacOS/Google\ Chrome \
//!   cargo test -p loom-cli --test live_frame_targeting -- --ignored --nocapture
//! ```

#![cfg(unix)]

mod common;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use common::daemon_test_harness::DaemonTestHarness;

/// Minimal static HTTP server on `127.0.0.1:0`. `router(path) -> Some(body)`
/// serves 200 text/html; `None` → 404. Loops until the test process exits.
fn spawn_static_server<F>(router: F) -> SocketAddr
where
    F: Fn(&str) -> Option<String> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture server");
    let addr = listener.local_addr().expect("local_addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req
                .split_whitespace()
                .nth(1)
                .map(|p| p.split('?').next().unwrap_or(p).to_string())
                .unwrap_or_default();
            let resp = match router(&path) {
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
    addr
}

const WIDGET_HTML: &str = "<!doctype html><html><head><title>widget</title></head><body>\
     <button id=\"b1\">B1</button><button id=\"b2\">B2</button>\
     <script>\
       document.getElementById('b1').addEventListener('click', function(){\
         document.getElementById('b2').id = 'b2done';\
       });\
     </script></body></html>";

#[test]
#[ignore = "real Chromium (no network); gated on LOOM_LIVE_E2E=1 + LOOM_CHROMIUM_PATH"]
fn cross_origin_iframe_click_lands_via_frame_locator() {
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

    // Origin B (widget) first — the parent needs B's URL to build the iframe src.
    let widget = spawn_static_server(|path| {
        (path == "/widget").then(|| WIDGET_HTML.to_string())
    });
    let parent_html = format!(
        "<!doctype html><html><head><title>parent</title></head><body>\
         <h1 id=\"parent\">parent</h1>\
         <iframe id=\"w\" src=\"http://{widget}/widget\" style=\"width:320px;height:200px;border:0\"></iframe>\
         </body></html>"
    );
    let parent = spawn_static_server(move |path| {
        (path == "/").then(|| parent_html.clone())
    });
    let parent_url = format!("http://{parent}/");

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

    // Land on the parent; the iframe loads the cross-origin widget.
    let nav = navigate(&harness, &sid, &parent_url, "settled");
    assert_eq!(
        nav["status"], "success",
        "navigate to the parent fixture must succeed; got {nav}"
    );

    // 1) Scope fence: a bare locator must NOT reach into the cross-origin iframe.
    let bare = click(&harness, &sid, "#b1");
    assert!(
        is_selector_not_found(&bare),
        "bare `#b1` must be selector_not_found (button lives in the cross-origin \
         iframe; a bare locator must not pierce an origin boundary); got {bare}"
    );

    // 2) Cross-origin reach: `frame=` descends into the in-process cross-origin
    //    iframe and resolves + clicks the element.
    let framed = click(&harness, &sid, "frame=#w >> css=#b1");
    assert_eq!(
        framed["status"], "success",
        "frame=#w >> css=#b1 must resolve + trusted-click inside the cross-origin \
         iframe; got {framed}"
    );
    assert!(
        framed["error"].is_null(),
        "successful cross-origin click must carry no error; got {framed}"
    );

    // 3) Proof the click LANDED: #b1's synchronous handler renamed #b2 → #b2done.
    //    Resolving #b2done (only present post-click) proves the trusted click hit
    //    the right element inside the cross-origin frame — no virtual-time re-arm
    //    needed because the handler is synchronous.
    let landed = click(&harness, &sid, "frame=#w >> css=#b2done");
    assert_eq!(
        landed["status"], "success",
        "#b2done exists only if #b1's click handler ran — the cross-origin trusted \
         click must have actually landed; got {landed}"
    );

    // 4) Confirm the mutation: the original #b2 id is gone.
    let gone = click(&harness, &sid, "frame=#w >> css=#b2");
    assert!(
        is_selector_not_found(&gone),
        "#b2 was renamed to #b2done by the handler, so #b2 must no longer resolve; got {gone}"
    );

    eprintln!("cross-origin iframe click: reach + land + scope-fence all OK");
    let _ = run_loom(&harness, &["session", "close", &sid]);
}

fn is_selector_not_found(receipt: &serde_json::Value) -> bool {
    receipt["status"] != "success"
        && serde_json::to_string(receipt)
            .map(|s| s.contains("selector_not_found"))
            .unwrap_or(false)
}

// ─── CLI helpers ─────────────────────────────────────────────────────────────

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

fn create_session(harness: &DaemonTestHarness) -> String {
    let out = run_loom(harness, &["session", "create", "--profile", "standard"]);
    assert_eq!(out.status, 0, "session create failed: stderr={}", out.stderr);
    let v: serde_json::Value = serde_json::from_str(&out.stdout)
        .unwrap_or_else(|e| panic!("session create not JSON: {e}; raw={:?}", out.stdout));
    v["session_id"].as_str().unwrap().to_string()
}

fn navigate(harness: &DaemonTestHarness, sid: &str, url: &str, until: &str) -> serde_json::Value {
    let out = run_loom(
        harness,
        &[
            "action", "web.navigate", "--session", sid, "--url", url, "--until", until,
        ],
    );
    serde_json::from_str(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "navigate({url}) stdout not JSON: {e}; status={} stderr={:?}",
            out.status, out.stderr
        )
    })
}

fn click(harness: &DaemonTestHarness, sid: &str, selector: &str) -> serde_json::Value {
    let out = run_loom(
        harness,
        &["action", "web.click", "--session", sid, "--selector", selector],
    );
    serde_json::from_str(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "click({selector}) stdout not JSON: {e}; status={} stderr={:?}",
            out.status, out.stderr
        )
    })
}

// ─── web-world provisioning (same as live_real_chrome_smoke) ──────────────────

fn provision_web_world(home: &Path) {
    let cfg_loom = home.join(".config").join("loom");
    let surfaces_dir = cfg_loom.join("surfaces");
    std::fs::create_dir_all(&surfaces_dir).unwrap();
    std::os::unix::fs::symlink(cwasm_path(), surfaces_dir.join("loom_surface_web.cwasm")).unwrap();
    let schemas_dir = cfg_loom.join("schemas").join("v1");
    std::fs::create_dir_all(&schemas_dir).unwrap();
    loom_cli::postinstall_runner::schema_step(&schemas_dir).unwrap();
    let permissive = r#"{"request":{"type":"object","additionalProperties":true},"response":{"type":"object","additionalProperties":true}}"#;
    for m in ["session.create", "session.close", "session.list", "session.validate"] {
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
        let cwasm = workspace_root().join("target/loom-frame-targeting-cwasm/loom_surface_web.cwasm");
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
