//! Live regression: faithful client-side entrance-animation rendering.
//!
//! Spec: specs/2026-06-09-faithful-entrance-animations. loom used to freeze the
//! in-page clock (`performance.now() === 0`), so JS-driven entrance animations
//! (framer-motion `whileInView`: inline `opacity:0` → animate to `1`) never
//! progressed and screenshots came back blank — while stock `--headless=new`
//! rendered the page. This guards the fix: loom must run the animation to its
//! final visible state and capture that frame, deterministically.
//!
//! Two hermetic fixture pages are served from a `127.0.0.1` `TcpListener` (loom's
//! URL allowlist permits http/https/about — NOT data:, so an in-process server
//! is the smallest fixture). Both animations time off `performance.now()` — the
//! exact API the bug froze — NOT the rAF timestamp argument.
//!
//!   T2 `reveal`   — opacity 0→1 over 500ms then STOPS. Pre-fix: performance.now
//!                   frozen → progress stuck at 0 → blank, never settles to
//!                   `reached`. Post-fix: completes → opacity≈1, captured.
//!   T3 `infinite` — random opacity every frame, never terminating. Must return
//!                   a TYPED settle verdict (never hang) and capture some frame.
//!
//! ## Double-gated — never runs in normal CI
//! - `#[ignore]`, so a plain `cargo test` skips it.
//! - Even under `--ignored` it early-returns UNLESS `LOOM_LIVE_E2E=1`.
//!
//! ## Running it
//! ```sh
//! cargo build -p loom-shims --features fake-chromium-bin --bin fake-chromium
//! cargo build --target wasm32-wasip2 -p loom-surface-web --release
//! LOOM_LIVE_E2E=1 \
//!   LOOM_CHROMIUM_PATH=/Applications/Google\ Chrome.app/Contents/MacOS/Google\ Chrome \
//!   cargo test -p loom-cli --test live_animation_render_regression -- --ignored --nocapture
//! ```

#![cfg(unix)]

mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::thread;

use common::daemon_test_harness::DaemonTestHarness;

/// A reveal that times off `performance.now()` and STOPS at opacity 1.
/// Pre-fix (frozen clock) this stalls at opacity 0 forever; post-fix it completes.
const REVEAL_HTML: &str = r#"<!doctype html><html><head><meta charset="utf-8">
<style>html,body{margin:0;background:#0b0b0f}#card{opacity:0;color:#fff;font:48px sans-serif;padding:80px}</style>
</head><body><div id="card" data-test="reveal">TEAM MEMBER</div>
<script>
  var el = document.getElementById('card');
  var DUR = 500, start = performance.now();
  function frame() {
    var p = Math.min((performance.now() - start) / DUR, 1);
    el.style.opacity = String(p);
    if (p < 1) requestAnimationFrame(frame);
  }
  requestAnimationFrame(frame);
</script></body></html>"#;

/// A never-terminating animation: random opacity every frame. Must never hang.
const INFINITE_HTML: &str = r#"<!doctype html><html><head><meta charset="utf-8">
<style>html,body{margin:0;background:#0b0b0f}#card{color:#fff;font:48px sans-serif;padding:80px}</style>
</head><body><div id="card" data-test="infinite">FOREVER</div>
<script>
  var el = document.getElementById('card');
  setInterval(function () {
    el.style.opacity = String((performance.now() % 1000) / 1000);
  }, 16);
</script></body></html>"#;

#[test]
#[ignore = "live + real Chromium; gated on LOOM_LIVE_E2E=1 + LOOM_CHROMIUM_PATH"]
fn entrance_animation_renders_final_visible_state() {
    if std::env::var("LOOM_LIVE_E2E").as_deref() != Ok("1") {
        eprintln!("skip: opt-in — set LOOM_LIVE_E2E=1 (and LOOM_CHROMIUM_PATH) to run");
        return;
    }
    let chromium = match std::env::var("LOOM_CHROMIUM_PATH") {
        Ok(p) if Path::new(&p).exists() => p,
        _ => {
            eprintln!("skip: LOOM_LIVE_E2E=1 but LOOM_CHROMIUM_PATH unset/missing");
            return;
        }
    };

    let reveal_url = serve(REVEAL_HTML);
    let infinite_url = serve(INFINITE_HTML);

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

    // ── T2: the reveal must reach a settled, final visible frame ──────────────
    let load = navigate(&harness, &sid, &reveal_url, "load");
    let settled = navigate(&harness, &sid, &reveal_url, "settled");

    assert_eq!(
        settled["settle_until"], "settled",
        "settled navigate must report settle_until=settled; got {settled}"
    );
    assert_eq!(
        settled["settle_outcome"], "reached",
        "REGRESSION: opacity:0→1 reveal never reached a settled state (got {:?}). \
         A frozen in-page clock stalls the animation so it mutates forever / stays \
         blank. settled receipt: {settled}",
        settled["settle_outcome"]
    );

    let load_hash = screenshot_hash(&load);
    let settled_hash = screenshot_hash(&settled);
    assert!(
        is_sha256_hex(&settled_hash),
        "settled must capture; got {settled}"
    );

    // PRIMARY acceptance: the animated element is in its final visible state.
    // (This is the faithful-render guarantee — opacity ~1, not stuck at 0.)
    let opacity = evaluate_f64(
        &harness,
        &sid,
        "parseFloat(getComputedStyle(document.querySelector('[data-test=reveal]')).opacity)",
    );
    eprintln!("reveal: opacity={opacity} load_hash={load_hash} settled_hash={settled_hash}");
    assert!(
        opacity >= 0.95,
        "REGRESSION: reveal element opacity after settle is {opacity} (< 0.95) — \
         the entrance animation did not render to its final visible state"
    );

    // Note: we deliberately do NOT assert load_hash != settled_hash. Under the
    // virtual-time clock the rAF reveal fast-forwards to completion BEFORE the
    // `--until load` capture too, so both legitimately show the final frame and
    // the hashes match. The opacity assertion above is the real acceptance; the
    // hashes are logged for diagnostics.

    // ── T3: a never-terminating animation must not hang ───────────────────────
    let inf = navigate(&harness, &sid, &infinite_url, "settled");
    let outcome = inf["settle_outcome"].as_str().unwrap_or("");
    assert!(
        matches!(
            outcome,
            "reached" | "timeout" | "dom_unstable" | "animations_unstable"
        ),
        "infinite animation must return a TYPED settle verdict (never hang); got {inf}"
    );
    assert!(
        is_sha256_hex(&screenshot_hash(&inf)),
        "infinite animation must still capture a (non-blank) frame; got {inf}"
    );
    eprintln!("infinite-animation settle_outcome={outcome}");

    // Determinism (NFR-DET-01 = REPLAY equality) is validated hermetically by the
    // loom-core replay/determinism suite — that is loom's actual guarantee, and it
    // stays green with this change (the manifest/replay path is untouched; only the
    // in-page clock moved to deterministic virtual time). Like loom's own
    // live_settle_regression, this live test deliberately does NOT assert
    // byte-identity across fresh real-Chrome sessions (that is not a loom
    // guarantee and is flaky under a system Chrome). Its job is faithful render +
    // no-hang, asserted above.

    // ── Acceptance #1: the original repro — mentiora.ai/team. After the fix no
    //    text-bearing reveal card should be stuck at opacity:0 (the bug left 11
    //    invisible). Live page (opt-in), so this is lenient: catch the
    //    many-stuck-cards regression, tolerate incidental hidden UI. ──────────
    // Opt-in (LOOM_LIVE_TEAM=1): the original repro against the LIVE site. Kept
    // out of the default run because a live network + a system Chrome make the
    // 4th navigate on this session flaky; the hermetic reveal/infinite checks
    // above are the reliable acceptance. When enabled, it is also infra-tolerant
    // (a navigate that fails to load is skipped, not failed).
    if std::env::var("LOOM_LIVE_TEAM").is_ok() {
        let nav = run_loom(
            &harness,
            &[
                "action",
                "web.navigate",
                "--session",
                &sid,
                "--url",
                "https://mentiora.ai/team",
                "--until",
                "settled",
            ],
        );
        if serde_json::from_str::<serde_json::Value>(&nav.stdout).is_err() {
            eprintln!("mentiora.ai/team: SKIP — navigate did not return a receipt (infra/network); stderr={:?}", nav.stderr);
            let _ = run_loom(&harness, &["session", "close", &sid]);
            return;
        }
        // Count text-bearing elements that are IN THE VIEWPORT (above the fold).
        // After the fix, in-viewport `whileInView` reveals must have fired
        // (opacity≈1); below-the-fold cards legitimately stay at opacity:0 until
        // scrolled (a real browser does the same), so we scope to the viewport.
        // Returns "<revealed>,<stuck>" for diagnostics.
        let probe = "(function(){var rev=0,stuck=0,vh=window.innerHeight;\
           Array.from(document.querySelectorAll('*')).forEach(function(e){\
             var r=e.getBoundingClientRect();\
             if(r.height>20 && r.top<vh && r.bottom>0 && (e.textContent||'').trim().length>0){\
               if(parseFloat(getComputedStyle(e).opacity) < 0.5) stuck++; else rev++;}});\
           return rev+','+stuck;})()";
        let out = run_loom(
            &harness,
            &[
                "action",
                "web.evaluate",
                "--session",
                &sid,
                "--expression",
                probe,
            ],
        );
        let v: serde_json::Value = serde_json::from_str(&out.stdout)
            .unwrap_or_else(|e| panic!("team probe not JSON: {e}; raw={:?}", out.stdout));
        let raw = v
            .get("return_value_json")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let pair = raw.trim_matches('"');
        let mut it = pair.split(',');
        let revealed: i64 = it.next().unwrap_or("0").parse().unwrap_or(0);
        let stuck: i64 = it.next().unwrap_or("0").parse().unwrap_or(0);
        eprintln!("mentiora.ai/team: in-viewport text elements revealed={revealed} stuck_opacity0={stuck}");
        // The original bug left the page BLANK (only the nav bar — a handful of
        // elements — visible, everything else stuck at opacity:0). After the fix
        // the reveal animations run, so the viewport shows substantial content and
        // the stuck elements are a small minority (fold-boundary cards / genuinely
        // hidden UI). Robust thresholds (live page): substantial content rendered,
        // and visible vastly outnumbers stuck.
        assert!(
            revealed >= 10,
            "REGRESSION: mentiora.ai/team rendered only {revealed} visible text element(s) in the \
             viewport — the original bug (blank page, only nav bar) is not fixed"
        );
        assert!(
            stuck * 4 <= revealed,
            "REGRESSION: {stuck} in-viewport text element(s) stuck at opacity:0 vs only {revealed} \
             revealed on mentiora.ai/team — above-the-fold entrance animations are not rendering"
        );
    }

    let _ = run_loom(&harness, &["session", "close", &sid]);
}

// ─── Minimal 127.0.0.1 fixture server (loom allowlist forbids data:) ─────────

/// Serve `body` on a fresh ephemeral 127.0.0.1 port; return the http:// URL.
/// One accept-thread answers every connection with the same page (the test
/// navigates a couple of times). Lives for the process duration.
fn serve(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
    let port = listener.local_addr().expect("local_addr").port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf); // drain the request line(s)
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
        }
    });
    format!("http://127.0.0.1:{port}/")
}

// ─── CLI helpers (trimmed copies of the sibling live e2e harness) ────────────

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

fn evaluate_f64(harness: &DaemonTestHarness, sid: &str, expr: &str) -> f64 {
    let out = run_loom(
        harness,
        &[
            "action",
            "web.evaluate",
            "--session",
            sid,
            "--expression",
            expr,
        ],
    );
    let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "evaluate stdout not JSON: {e}; status={} raw={:?}",
            out.status, out.stdout
        )
    });
    // The receipt surfaces the evaluate result as `return_value_json` (a
    // JSON-encoded string, e.g. "1" or "0.997"). Accept a few shapes defensively.
    let as_num = |x: &serde_json::Value| -> Option<f64> {
        x.as_f64()
            .or_else(|| x.as_str().and_then(|s| s.trim_matches('"').parse().ok()))
    };
    v.get("return_value_json")
        .or_else(|| v.get("return_value"))
        .or_else(|| v.get("result"))
        .or_else(|| v.get("value"))
        .and_then(as_num)
        .unwrap_or_else(|| panic!("evaluate did not yield a numeric result: {v}"))
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

// ─── Hermetic web-world provisioning (same paths as the sibling e2e) ─────────

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
        let cwasm = workspace_root().join("target/loom-live-anim-cwasm/loom_surface_web.cwasm");
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
