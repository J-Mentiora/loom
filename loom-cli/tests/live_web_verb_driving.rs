//! Real-Chrome reproduction + regression guard for the three web-verb driving
//! blockers filed against 0.13.0 (Mentiora CX hand-drive):
//!
//! - **B1** — `web.set_input_files` surface-traps on a valid file under
//!   `LOOM_UPLOAD_ROOT` (the `DOM.setFileInputFiles` dispatch step), after the
//!   upload-guard validation passes. Acceptance: the file attaches
//!   (`input.files.length === 1`) without trapping.
//! - **P1** — a trusted `web.click` on a non-navigation, async-triggering button
//!   wedges the daemon (the click's effect needs virtual time to advance, which
//!   the input path never re-arms — the non-navigation cousin of #219).
//!   Acceptance: the click advances without a multi-second wedge.
//! - **B2** — `web.press_key` (Enter) on a cross-origin iframe composer crashes
//!   the shim subprocess. Acceptance: press_key inside a cross-origin iframe does
//!   not crash the shim; it accepts the `frame=` locator grammar.
//!
//! ## Double-gated — never runs in normal CI
//! - `#[ignore]`; even under `--ignored` early-returns unless `LOOM_LIVE_E2E=1`.
//!
//! ## Running it
//! ```sh
//! cargo build --target wasm32-wasip2 -p loom-surface-web --release
//! LOOM_LIVE_E2E=1 \
//!   LOOM_CHROMIUM_PATH=/Applications/Google\ Chrome.app/Contents/MacOS/Google\ Chrome \
//!   cargo test -p loom-cli --test live_web_verb_driving -- --ignored --nocapture
//! ```

#![cfg(unix)]

mod common;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

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

fn live_gate() -> Option<String> {
    if std::env::var("LOOM_LIVE_E2E").as_deref() != Ok("1") {
        eprintln!("skip: set LOOM_LIVE_E2E=1 + LOOM_CHROMIUM_PATH to run");
        return None;
    }
    match std::env::var("LOOM_CHROMIUM_PATH") {
        Ok(p) if Path::new(&p).exists() => Some(p),
        _ => {
            eprintln!("skip: LOOM_CHROMIUM_PATH unset/missing");
            None
        }
    }
}

fn harness_for(chromium: &str) -> DaemonTestHarness {
    DaemonTestHarness::new()
        .env("LOOM_CHROMIUM_PATH", chromium)
        .env(
            "LOOM_CHROMIUM_EXTRA_FLAGS",
            "--no-sandbox --disable-dev-shm-usage --use-mock-keychain --password-store=basic",
        )
        .with_ready_timeout(Duration::from_secs(30))
}

// ─── B1: set_input_files on a valid file under LOOM_UPLOAD_ROOT ───────────────

const UPLOAD_HTML: &str = "<!doctype html><html><head><title>upload</title></head><body>\
     <input type=\"file\" id=\"up\">\
     <input type=\"file\" id=\"multi\" multiple>\
     </body></html>";

#[test]
#[ignore = "real Chromium (no network); gated on LOOM_LIVE_E2E=1 + LOOM_CHROMIUM_PATH"]
fn set_input_files_attaches_a_valid_file_without_trapping() {
    let Some(chromium) = live_gate() else { return };

    let app = spawn_static_server(|path| (path == "/").then(|| UPLOAD_HTML.to_string()));
    let app_url = format!("http://{app}/");

    let mut harness = harness_for(&chromium);
    // Fixtures live DIRECTLY under LOOM_UPLOAD_ROOT (loom authorizes by resolving
    // the path against the root).
    let upload_root = harness.home().join("uploads");
    std::fs::create_dir_all(&upload_root).unwrap();
    let fixture = upload_root.join("returns-policy.md");
    std::fs::write(
        &fixture,
        b"# Returns policy\nItems may be returned within 30 days.\n",
    )
    .unwrap();
    let fixture2 = upload_root.join("warranty.md");
    std::fs::write(&fixture2, b"# Warranty\n12 months.\n").unwrap();
    harness = harness.env("LOOM_UPLOAD_ROOT", &upload_root);

    provision_web_world(harness.home());
    harness.start();
    let sid = create_session(&harness);

    let nav = navigate(&harness, &sid, &app_url, "settled");
    assert_eq!(nav["status"], "success", "navigate must succeed; got {nav}");

    // Single-file attach — the exact step the knowledge-grounding journey needs.
    let single = set_input_files(&harness, &sid, "#up", &[fixture.to_str().unwrap()]);
    assert_eq!(
        single["status"], "success",
        "set_input_files with a valid file under LOOM_UPLOAD_ROOT must NOT trap; got {single}"
    );
    let after = eval_text(&harness, &sid, "document.getElementById('up').files.length");
    assert!(
        json_contains(&after, "1"),
        "the file must be attached (files.length === 1); got {after}"
    );

    // Multi-file batch.
    let multi = set_input_files(
        &harness,
        &sid,
        "#multi",
        &[fixture.to_str().unwrap(), fixture2.to_str().unwrap()],
    );
    assert_eq!(
        multi["status"], "success",
        "multi-file set_input_files must succeed; got {multi}"
    );
    let after_multi = eval_text(
        &harness,
        &sid,
        "document.getElementById('multi').files.length",
    );
    assert!(
        json_contains(&after_multi, "2"),
        "both files must be attached (files.length === 2); got {after_multi}"
    );

    eprintln!("set_input_files: single + multi attach OK");
    let _ = run_loom(&harness, &["session", "close", &sid]);
}

// B1 variant — the input is INJECTED via web.evaluate (the filer's exact words:
// "a trivial injected <input type=file> on example.com"), not present in the
// static HTML. A dynamically-created node exercises the live-DOM querySelector
// path differently from a parse-time node.
#[test]
#[ignore = "real Chromium (no network); gated on LOOM_LIVE_E2E=1 + LOOM_CHROMIUM_PATH"]
fn set_input_files_on_a_dynamically_injected_input() {
    let Some(chromium) = live_gate() else { return };

    // Blank page; the input does not exist until we inject it.
    let app = spawn_static_server(|path| {
        (path == "/").then(|| "<!doctype html><html><body><h1>blank</h1></body></html>".to_string())
    });
    let app_url = format!("http://{app}/");

    let mut harness = harness_for(&chromium);
    let upload_root = harness.home().join("uploads");
    std::fs::create_dir_all(&upload_root).unwrap();
    let fixture = upload_root.join("returns-policy.md");
    std::fs::write(&fixture, b"# Returns policy\n30 days.\n").unwrap();
    harness = harness.env("LOOM_UPLOAD_ROOT", &upload_root);
    provision_web_world(harness.home());
    harness.start();
    let sid = create_session(&harness);

    let nav = navigate(&harness, &sid, &app_url, "settled");
    assert_eq!(nav["status"], "success", "navigate must succeed; got {nav}");

    // Inject the file input dynamically (the filer's repro shape).
    let inject = eval_text(
        &harness,
        &sid,
        "(function(){var i=document.createElement('input');i.type='file';i.id='inj';document.body.appendChild(i);return document.querySelectorAll('input[type=file]').length;})()",
    );
    assert!(
        json_contains(&inject, "1"),
        "the injected input must exist; got {inject}"
    );

    let attached = set_input_files(&harness, &sid, "#inj", &[fixture.to_str().unwrap()]);
    assert_eq!(
        attached["status"], "success",
        "set_input_files on a dynamically-injected input must NOT trap; got {attached}"
    );
    let after = eval_text(
        &harness,
        &sid,
        "document.getElementById('inj').files.length",
    );
    assert!(
        json_contains(&after, "1"),
        "the file must attach to the injected input (files.length===1); got {after}"
    );

    eprintln!("set_input_files on injected input: attach OK");
    let _ = run_loom(&harness, &["session", "close", &sid]);
}

// B1 ROOT-CAUSE probe — the filer's repro uses `--selector "css=#t"` (the
// locator-grammar form click/type accept). set_input_files passes the selector
// STRAIGHT to DOM.querySelector without parse_locator, so `css=#t` reaches Chrome
// as a literal (invalid) CSS selector. This pins whether the `css=`/`text=`/
// `frame=` grammar is the trigger.
#[test]
#[ignore = "real Chromium (no network); gated on LOOM_LIVE_E2E=1 + LOOM_CHROMIUM_PATH"]
fn set_input_files_selector_grammar_probe() {
    let Some(chromium) = live_gate() else { return };

    let app = spawn_static_server(|path| (path == "/").then(|| UPLOAD_HTML.to_string()));
    let app_url = format!("http://{app}/");

    let mut harness = harness_for(&chromium);
    let upload_root = harness.home().join("uploads");
    std::fs::create_dir_all(&upload_root).unwrap();
    let fixture = upload_root.join("repro.txt");
    std::fs::write(&fixture, b"hello from loom repro\n").unwrap();
    harness = harness.env("LOOM_UPLOAD_ROOT", &upload_root);
    provision_web_world(harness.home());
    harness.start();
    let sid = create_session(&harness);

    let nav = navigate(&harness, &sid, &app_url, "settled");
    assert_eq!(nav["status"], "success", "navigate must succeed; got {nav}");

    let fpath = fixture.to_str().unwrap();

    // The filer's exact form: a `css=`-prefixed locator. Before the fix this was
    // passed RAW to DOM.querySelector (no parse_locator) and surface_trapped even
    // for a valid file under a valid LOOM_UPLOAD_ROOT. It must now resolve like
    // web.click/web.type and attach.
    let css = set_input_files(&harness, &sid, "css=#up", &[fpath]);
    assert_eq!(
        css["status"], "success",
        "css= grammar selector must resolve + attach (not surface_trap); got {css}"
    );
    let after_css = eval_text(&harness, &sid, "document.getElementById('up').files.length");
    assert!(
        json_contains(&after_css, "1"),
        "css= selector must attach the file (files.length===1); got {after_css}"
    );

    // Bare CSS still works (unchanged).
    let bare = set_input_files(&harness, &sid, "#multi", &[fpath]);
    assert_eq!(
        bare["status"], "success",
        "bare selector must still attach; got {bare}"
    );

    // A genuine no-match must NOT crash the daemon (it stays serviceable).
    let nomatch = set_input_files_raw(&harness, &sid, "#does-not-exist", &[fpath]);
    assert_ne!(
        nomatch.status,
        0,
        "a no-match selector must be a non-success outcome; got {nomatch:?}",
        nomatch = (nomatch.status, &nomatch.stdout, &nomatch.stderr)
    );
    let alive = eval_text(&harness, &sid, "1 + 1");
    assert!(
        json_contains(&alive, "2"),
        "daemon must stay serviceable after a no-match set_input_files; got {alive}"
    );

    eprintln!(
        "set_input_files selector grammar: css= resolves + attaches; no-match is serviceable"
    );
    let _ = run_loom(&harness, &["session", "close", &sid]);
}

// ─── web.wait locator grammar (regression for the reported js_throw) ──────────
//
// The filer's repro: `web.wait --selector "text=Ready 1"` → js_throw, while
// `web.click --selector "text=Ready 1"` → success. web.wait passed the raw
// locator to `querySelector`, so any non-CSS grammar (text=/role=/css=) raised a
// SyntaxError. web.wait is now host-intercepted and resolves the SAME grammar as
// web.click. This pins the grammar + the timeout→wait_predicate_false outcome.
//
// NOTE: the *delayed-appearance* polling semantics are unit-tested in
// loom-host `senders.rs` (`poll_resolves_on_delayed_appearance`) on a paused
// clock — under determinism a `setTimeout`-injected element will NOT fire during
// web.wait's no-virtual-time-budget poll (the deferred virtual-time re-arm, P1),
// so the genuine "appears after a delay" case belongs at the unit level.
const WAIT_GRAMMAR_HTML: &str =
    "<!doctype html><html><head><title>wait grammar</title></head><body>\
     <button id=\"ready\">Ready 1</button>\
     <div id=\"box\">box contents</div>\
     </body></html>";

#[test]
#[ignore = "real Chromium (no network); gated on LOOM_LIVE_E2E=1 + LOOM_CHROMIUM_PATH"]
fn web_wait_accepts_locator_grammar_not_just_css() {
    let Some(chromium) = live_gate() else { return };

    let app = spawn_static_server(|path| (path == "/").then(|| WAIT_GRAMMAR_HTML.to_string()));
    let app_url = format!("http://{app}/");

    let mut harness = harness_for(&chromium);
    provision_web_world(harness.home());
    harness.start();
    let sid = create_session(&harness);

    let nav = navigate(&harness, &sid, &app_url, "settled");
    assert_eq!(nav["status"], "success", "navigate must succeed; got {nav}");

    // The reported repro: a `text=` locator. Before the fix this threw js_throw
    // (raw querySelector("text=Ready 1") → SyntaxError). It must now resolve.
    let by_text = web_wait(&harness, &sid, "text=Ready 1", Some(5000));
    assert_eq!(
        by_text["status"], "success",
        "text= locator must resolve in web.wait (not js_throw); got {by_text}"
    );

    // `role=` grammar resolves too (ARIA role + accessible name).
    let by_role = web_wait(&harness, &sid, "role=button[name=\"Ready 1\"]", Some(5000));
    assert_eq!(
        by_role["status"], "success",
        "role= locator must resolve in web.wait; got {by_role}"
    );

    // Bare CSS still works (back-compat — unchanged presence semantics).
    let by_css = web_wait(&harness, &sid, "#box", Some(5000));
    assert_eq!(
        by_css["status"], "success",
        "bare CSS selector must still resolve; got {by_css}"
    );

    // A genuine no-match polls to the deadline and reports the typed
    // wait_predicate_false — NOT js_throw, NOT a crash. Short timeout keeps it quick.
    let miss = web_wait(&harness, &sid, "text=Definitely Not Present", Some(800));
    assert_ne!(
        miss["status"], "success",
        "a never-appearing locator must not report success; got {miss}"
    );
    assert!(
        serde_json::to_string(&miss)
            .unwrap()
            .contains("wait_predicate_false"),
        "timeout must surface the typed wait_predicate_false kind; got {miss}"
    );

    // Daemon stays serviceable after the timeout.
    let alive = eval_text(&harness, &sid, "1 + 1");
    assert!(
        json_contains(&alive, "2"),
        "daemon must stay serviceable after a web.wait timeout; got {alive}"
    );

    eprintln!(
        "web.wait grammar: text=/role=/css= resolve; no-match → wait_predicate_false; serviceable"
    );
    let _ = run_loom(&harness, &["session", "close", &sid]);
}

// ─── P1 characterization: trusted click on an async button (NOT a standalone bug) ──
//
// The filer's P1 (a 30s trusted-click wedge) does NOT reproduce in isolation —
// neither the filer's three attempts (fetch/setTimeout/EventSource) nor this one
// wedge. Under determinism the click returns PROMPTLY; the click handler's
// deferred `setTimeout` mutation simply doesn't run until a virtual-time budget is
// armed — which is the determinism model, not a defect. `web.wait_for` arms that
// budget (#219), so the documented pattern is: trusted click → web.wait_for. This
// test pins that contract (no wedge; deferred work advances after wait_for).
const ASYNC_BTN_HTML: &str = "<!doctype html><html><head><title>async</title></head><body>\
     <button id=\"add\">Add document</button>\
     <span id=\"state\">idle</span>\
     <script>\
       document.getElementById('add').addEventListener('click', function(){\
         setTimeout(function(){\
           document.getElementById('state').textContent = 'added';\
         }, 50);\
       });\
     </script></body></html>";

#[test]
#[ignore = "real Chromium (no network); gated on LOOM_LIVE_E2E=1 + LOOM_CHROMIUM_PATH"]
fn trusted_click_on_async_button_returns_promptly_and_settles_via_wait_for() {
    let Some(chromium) = live_gate() else { return };

    let app = spawn_static_server(|path| (path == "/").then(|| ASYNC_BTN_HTML.to_string()));
    let app_url = format!("http://{app}/");

    let mut harness = harness_for(&chromium);
    provision_web_world(harness.home());
    harness.start();
    let sid = create_session(&harness);

    let nav = navigate(&harness, &sid, &app_url, "settled");
    assert_eq!(nav["status"], "success", "navigate must succeed; got {nav}");

    // The click returns promptly — the isolated case never wedges 30s.
    let t0 = Instant::now();
    let clicked = click(&harness, &sid, "#add");
    let elapsed = t0.elapsed();
    assert_eq!(
        clicked["status"], "success",
        "trusted click on the async button must succeed; got {clicked}"
    );
    assert!(
        elapsed < Duration::from_secs(15),
        "trusted click on an async button must not wedge; took {elapsed:?}"
    );

    // web.wait_for arms a virtual-time budget so the deferred handler runs — the
    // documented pattern for advancing post-interaction async work.
    let waited = wait_for(&harness, &sid, "settled");
    assert_eq!(
        waited["status"], "success",
        "wait_for must succeed; got {waited}"
    );
    let state = eval_text(
        &harness,
        &sid,
        "document.getElementById('state').textContent",
    );
    assert!(
        json_contains(&state, "added"),
        "after web.wait_for, the deferred click handler must have advanced (state=added); got {state}"
    );

    eprintln!("trusted click on async button: prompt ({elapsed:?}); deferred work advanced after wait_for");
    let _ = run_loom(&harness, &["session", "close", &sid]);
}

// ─── B2: press_key on a cross-origin iframe composer must not crash the shim ──

const COMPOSER_WIDGET_HTML: &str = "<!doctype html><html><head><title>widget</title></head><body>\
     <textarea id=\"composer\"></textarea>\
     <div id=\"sent\">no</div>\
     <script>\
       document.getElementById('composer').addEventListener('keydown', function(e){\
         if (e.key === 'Enter') { document.getElementById('sent').textContent = 'yes'; }\
       });\
     </script></body></html>";

#[test]
#[ignore = "real Chromium (no network); gated on LOOM_LIVE_E2E=1 + LOOM_CHROMIUM_PATH"]
fn press_key_in_cross_origin_iframe_does_not_crash_shim() {
    let Some(chromium) = live_gate() else { return };

    let widget =
        spawn_static_server(|path| (path == "/widget").then(|| COMPOSER_WIDGET_HTML.to_string()));
    let parent_html = format!(
        "<!doctype html><html><head><title>parent</title></head><body>\
         <iframe id=\"w\" src=\"http://{widget}/widget\" style=\"width:320px;height:200px;border:0\"></iframe>\
         </body></html>"
    );
    let parent = spawn_static_server(move |path| (path == "/").then(|| parent_html.clone()));
    let parent_url = format!("http://{parent}/");

    let mut harness = harness_for(&chromium);
    provision_web_world(harness.home());
    harness.start();
    let sid = create_session(&harness);

    let nav = navigate(&harness, &sid, &parent_url, "settled");
    assert_eq!(nav["status"], "success", "navigate must succeed; got {nav}");

    // Focus the composer inside the cross-origin iframe (web.type already
    // supports frame= and is proven to work), then press Enter targeting the
    // same frame-scoped field. press_key must accept frame= grammar and must
    // NOT crash the shim.
    let typed = type_text(&harness, &sid, "frame=#w >> css=#composer", "hello");
    assert_eq!(
        typed["status"], "success",
        "web.type into the cross-origin composer must succeed; got {typed}"
    );

    let pressed = press_key(&harness, &sid, "Enter", Some("frame=#w >> css=#composer"));
    // The shim must still be alive (no `shim response oneshot dropped`).
    assert!(
        !json_contains(&pressed, "subprocess gone") && !json_contains(&pressed, "shim_failure"),
        "press_key in a cross-origin iframe must not crash the shim; got {pressed}"
    );
    assert_eq!(
        pressed["status"], "success",
        "press_key Enter in the cross-origin composer must succeed; got {pressed}"
    );

    // Proof the keydown landed in the frame: the handler set #sent → yes.
    let sent = click(&harness, &sid, "frame=#w >> css=#sent");
    assert_eq!(
        sent["status"], "success",
        "the #sent node must resolve in the frame; got {sent}"
    );

    // The daemon must still serve a follow-up action (shim alive).
    let alive = eval_text(&harness, &sid, "1 + 1");
    assert!(
        json_contains(&alive, "2"),
        "daemon/shim must still be responsive after press_key; got {alive}"
    );

    eprintln!("press_key in cross-origin iframe: no shim crash");
    let _ = run_loom(&harness, &["session", "close", &sid]);
}

// B2 variant — a contenteditable composer (the common chat-widget shape) with an
// Enter keydown handler, and press_key WITHOUT a selector (relying on the focus
// left by a prior web.type, as the reporter drove it).
const CE_WIDGET_HTML: &str = "<!doctype html><html><head><title>widget</title></head><body>\
     <div id=\"composer\" contenteditable=\"true\" style=\"border:1px solid #ccc;min-height:40px\"></div>\
     <div id=\"sent\">no</div>\
     <script>\
       document.getElementById('composer').addEventListener('keydown', function(e){\
         if (e.key === 'Enter') { e.preventDefault(); document.getElementById('sent').textContent='yes'; }\
       });\
     </script></body></html>";

#[test]
#[ignore = "real Chromium (no network); gated on LOOM_LIVE_E2E=1 + LOOM_CHROMIUM_PATH"]
fn press_key_sans_selector_on_contenteditable_iframe_composer() {
    let Some(chromium) = live_gate() else { return };

    let widget =
        spawn_static_server(|path| (path == "/widget").then(|| CE_WIDGET_HTML.to_string()));
    let parent_html = format!(
        "<!doctype html><html><head><title>parent</title></head><body>\
         <iframe id=\"w\" src=\"http://{widget}/widget\" style=\"width:320px;height:200px;border:0\"></iframe>\
         </body></html>"
    );
    let parent = spawn_static_server(move |path| (path == "/").then(|| parent_html.clone()));
    let parent_url = format!("http://{parent}/");

    let mut harness = harness_for(&chromium);
    provision_web_world(harness.home());
    harness.start();
    let sid = create_session(&harness);

    let nav = navigate(&harness, &sid, &parent_url, "settled");
    assert_eq!(nav["status"], "success", "navigate must succeed; got {nav}");

    // Type into the contenteditable composer (focuses it inside the frame).
    let typed = type_text(&harness, &sid, "frame=#w >> css=#composer", "hello");
    assert_eq!(typed["status"], "success", "type must succeed; got {typed}");

    // press_key Enter WITHOUT a selector — relies on the focus from the prior
    // type. This is the exact shape the reporter drove (and saw crash).
    let pressed = press_key(&harness, &sid, "Enter", None);
    assert!(
        !json_contains(&pressed, "subprocess gone") && !json_contains(&pressed, "shim_failure"),
        "sans-selector press_key after typing into the iframe must not crash the shim; got {pressed}"
    );

    let alive = eval_text(&harness, &sid, "1 + 1");
    assert!(
        json_contains(&alive, "2"),
        "daemon/shim must still be responsive after sans-selector press_key; got {alive}"
    );

    eprintln!("sans-selector press_key on contenteditable composer: no shim crash ({pressed})");
    let _ = run_loom(&harness, &["session", "close", &sid]);
}

// B2 variant — force the cross-origin widget into its OWN renderer process
// (`--isolate-origins`), making it a true OUT-OF-PROCESS iframe (OOPIF), the
// shape a real chat widget on a different domain has. press_key (and type/click)
// targeting an OOPIF must degrade cleanly (selector_not_found), never crash the
// shim subprocess.
#[test]
#[ignore = "real Chromium (no network); gated on LOOM_LIVE_E2E=1 + LOOM_CHROMIUM_PATH"]
fn press_key_on_oopif_does_not_crash_shim() {
    let Some(chromium) = live_gate() else { return };

    let widget =
        spawn_static_server(|path| (path == "/widget").then(|| COMPOSER_WIDGET_HTML.to_string()));
    let widget_origin = format!("http://{widget}");
    let parent_html = format!(
        "<!doctype html><html><head><title>parent</title></head><body>\
         <iframe id=\"w\" src=\"http://{widget}/widget\" style=\"width:320px;height:200px;border:0\"></iframe>\
         </body></html>"
    );
    let parent = spawn_static_server(move |path| (path == "/").then(|| parent_html.clone()));
    let parent_url = format!("http://{parent}/");

    // --isolate-origins forces the widget origin OOP even though it's same-site.
    let mut harness = DaemonTestHarness::new()
        .env("LOOM_CHROMIUM_PATH", &chromium)
        .env(
            "LOOM_CHROMIUM_EXTRA_FLAGS",
            format!(
                "--no-sandbox --disable-dev-shm-usage --use-mock-keychain --password-store=basic \
                 --isolate-origins={widget_origin} --site-per-process"
            ),
        )
        .with_ready_timeout(Duration::from_secs(30));
    provision_web_world(harness.home());
    harness.start();
    let sid = create_session(&harness);

    let nav = navigate(&harness, &sid, &parent_url, "settled");
    assert_eq!(nav["status"], "success", "navigate must succeed; got {nav}");

    // press_key targeting the OOPIF composer — must not crash the shim.
    let pressed = press_key(&harness, &sid, "Enter", Some("frame=#w >> css=#composer"));
    assert!(
        !json_contains(&pressed, "subprocess gone") && !json_contains(&pressed, "shim_failure"),
        "press_key on an OOPIF must not crash the shim; got {pressed}"
    );

    // sans-selector press_key too.
    let pressed2 = press_key(&harness, &sid, "Enter", None);
    assert!(
        !json_contains(&pressed2, "subprocess gone") && !json_contains(&pressed2, "shim_failure"),
        "sans-selector press_key with an OOPIF present must not crash the shim; got {pressed2}"
    );

    // Shim still alive.
    let alive = eval_text(&harness, &sid, "1 + 1");
    assert!(
        json_contains(&alive, "2"),
        "daemon/shim must still be responsive after OOPIF press_key; got {alive}"
    );

    eprintln!("press_key on OOPIF: no shim crash (pressed={pressed}, pressed2={pressed2})");
    let _ = run_loom(&harness, &["session", "close", &sid]);
}

// ── no_determinism repeated-navigate to a cross-origin-iframe SPA stays fast ──
// Regression for the staging Auth0 wedge: a `--no-determinism` (real wall-clock)
// session navigating repeatedly to a page that injects a hidden cross-origin
// iframe (mimicking the Auth0 `prompt=none` silent-auth iframe) used to WEDGE on
// the 2nd navigate. Root cause: navigate/`wait_for` armed + awaited a virtual-time
// budget whenever the global capture flag (`virtual_time_enabled()`) was on — even
// in a `--no-determinism` session that never pinned the clock — and
// `virtualTimeBudgetExpired` never reliably fired on the real-clock page (the
// cross-origin iframe keeps network fetches pending under
// `pauseIfNetworkFetchesPending`), so every navigate burned its full timeout and
// the 2nd one wedged the session. Fixed by gating the virtual-time path on
// whether the clock was ACTUALLY pinned at inject (`clock_pinned`), so
// `--no-determinism` takes the clean real-clock load+settle path.
//
// Guard: under `--no-determinism`, every navigate to such a page succeeds, the
// shim stays responsive, and each navigate completes well under the per-call
// deadline (it no longer stalls the full timeout waiting for a vt budget that
// can't expire). Determinism-ON behavior is unchanged (covered by
// live_client_redirect_reattach + integration_navigate_settle_e2e).
#[test]
#[ignore = "real Chromium (no network); gated on LOOM_LIVE_E2E=1 + LOOM_CHROMIUM_PATH"]
fn no_determinism_repeated_navigate_to_cross_origin_iframe_spa_stays_fast() {
    let Some(chromium) = live_gate() else { return };

    // Origin B: the "silent-auth" iframe content (forced into its own renderer).
    let auth = spawn_static_server(|path| {
        (path == "/silent-auth").then(|| {
            "<!doctype html><html><head><title>silent-auth</title></head>\
             <body>auth<script>/* token re-check */</script></body></html>"
                .to_string()
        })
    });
    let auth_origin = format!("http://{auth}");

    // Origin A: the app whose SDK injects the hidden cross-origin iframe on load.
    let app_html = format!(
        "<!doctype html><html><head><title>app</title></head><body><h1 id=\"app\">app</h1>\
         <script>\
           var f=document.createElement('iframe');\
           f.id='silent-auth';f.style.display='none';\
           f.src='http://{auth}/silent-auth';\
           document.body.appendChild(f);\
         </script></body></html>"
    );
    let app = spawn_static_server(move |path| (path == "/").then(|| app_html.clone()));
    let app_url = format!("http://{app}/");

    // --isolate-origins forces the auth origin out-of-process (true OOPIF).
    let mut harness = DaemonTestHarness::new()
        .env("LOOM_CHROMIUM_PATH", &chromium)
        .env(
            "LOOM_CHROMIUM_EXTRA_FLAGS",
            format!(
                "--no-sandbox --disable-dev-shm-usage --use-mock-keychain --password-store=basic \
                 --isolate-origins={auth_origin} --site-per-process"
            ),
        )
        // Shim stderr (incl. any panic backtrace) is drained into daemon.stderr.
        .env("RUST_BACKTRACE", "full")
        .with_ready_timeout(Duration::from_secs(30));
    provision_web_world(harness.home());
    harness.start();

    // --no-determinism: real wall-clock (the studio's mode, where the bug lived).
    let out = run_loom(
        &harness,
        &[
            "session",
            "create",
            "--no-determinism",
            "--profile",
            "standard",
        ],
    );
    assert_eq!(out.status, 0, "session create failed: {}", out.stderr);
    let sid = serde_json::from_str::<serde_json::Value>(&out.stdout).expect("session create JSON")
        ["session_id"]
        .as_str()
        .expect("session_id")
        .to_string();

    let daemon_stderr = harness.home().join("daemon.stderr");
    for i in 1..=4 {
        let t0 = Instant::now();
        let nav = navigate(&harness, &sid, &app_url, "settled");
        let elapsed = t0.elapsed();
        let log = std::fs::read_to_string(&daemon_stderr).unwrap_or_default();
        assert_eq!(
            nav["status"], "success",
            "navigate #{i} must succeed (no wedge); got {nav}\n--- daemon.stderr ---\n{log}"
        );
        // Pre-fix, navigate stalled the full settle timeout (~10-20s) waiting for a
        // virtualTimeBudgetExpired that never came. Post-fix it's a real-clock
        // settle (sub-second on this trivial page). 10s is a generous ceiling that
        // still catches the regression.
        assert!(
            elapsed < Duration::from_secs(10),
            "navigate #{i} took {elapsed:?} — the no_determinism virtual-time stall is back\n\
             --- daemon.stderr ---\n{log}"
        );
        let alive = eval_text(&harness, &sid, "1 + 1");
        assert!(
            json_contains(&alive, "2"),
            "shim must stay responsive after navigate #{i}; got {alive}\n\
             --- daemon.stderr ---\n{log}"
        );
    }

    eprintln!("no_determinism repeated navigate to cross-origin-iframe SPA: fast + responsive ×4");
    let _ = run_loom(&harness, &["session", "close", &sid]);
}

// ─── helpers (mirrors live_frame_targeting) ───────────────────────────────────

fn json_contains(v: &serde_json::Value, needle: &str) -> bool {
    serde_json::to_string(v)
        .map(|s| s.contains(needle))
        .unwrap_or(false)
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

fn create_session(harness: &DaemonTestHarness) -> String {
    let out = run_loom(harness, &["session", "create", "--profile", "standard"]);
    assert_eq!(
        out.status, 0,
        "session create failed: stderr={}",
        out.stderr
    );
    let v: serde_json::Value = serde_json::from_str(&out.stdout)
        .unwrap_or_else(|e| panic!("session create not JSON: {e}; raw={:?}", out.stdout));
    v["session_id"].as_str().unwrap().to_string()
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
        &[
            "action",
            "web.click",
            "--session",
            sid,
            "--selector",
            selector,
        ],
    );
    serde_json::from_str(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "click({selector}) stdout not JSON: {e}; status={} stderr={:?}",
            out.status, out.stderr
        )
    })
}

fn eval_text(harness: &DaemonTestHarness, sid: &str, expr: &str) -> serde_json::Value {
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
    serde_json::from_str(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "evaluate stdout not JSON: {e}; status={} stderr={:?}",
            out.status, out.stderr
        )
    })
}

fn type_text(
    harness: &DaemonTestHarness,
    sid: &str,
    selector: &str,
    text: &str,
) -> serde_json::Value {
    let out = run_loom(
        harness,
        &[
            "action",
            "web.type",
            "--session",
            sid,
            "--selector",
            selector,
            "--text",
            text,
        ],
    );
    serde_json::from_str(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "type({selector}) stdout not JSON: {e}; status={} stderr={:?}",
            out.status, out.stderr
        )
    })
}

fn wait_for(harness: &DaemonTestHarness, sid: &str, until: &str) -> serde_json::Value {
    let out = run_loom(
        harness,
        &["action", "web.wait_for", "--session", sid, "--until", until],
    );
    serde_json::from_str(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "wait_for stdout not JSON: {e}; status={} stderr={:?}",
            out.status, out.stderr
        )
    })
}

fn web_wait(
    harness: &DaemonTestHarness,
    sid: &str,
    selector: &str,
    timeout_ms: Option<u64>,
) -> serde_json::Value {
    let mut args = vec![
        "action".to_string(),
        "web.wait".to_string(),
        "--session".to_string(),
        sid.to_string(),
        "--selector".to_string(),
        selector.to_string(),
    ];
    if let Some(t) = timeout_ms {
        args.push("--timeout_ms".to_string());
        args.push(t.to_string());
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run_loom(harness, &arg_refs);
    serde_json::from_str(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "web.wait({selector}) stdout not JSON: {e}; status={} stderr={:?}",
            out.status, out.stderr
        )
    })
}

fn press_key(
    harness: &DaemonTestHarness,
    sid: &str,
    key: &str,
    selector: Option<&str>,
) -> serde_json::Value {
    let mut args = vec!["action", "web.press_key", "--session", sid, "--key", key];
    if let Some(sel) = selector {
        args.push("--selector");
        args.push(sel);
    }
    let out = run_loom(harness, &args);
    serde_json::from_str(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "press_key({key}) stdout not JSON: {e}; status={} stderr={:?}",
            out.status, out.stderr
        )
    })
}

fn set_input_files_raw(
    harness: &DaemonTestHarness,
    sid: &str,
    selector: &str,
    paths: &[&str],
) -> CliOutput {
    let paths_json = serde_json::to_string(paths).unwrap();
    run_loom(
        harness,
        &[
            "action",
            "web.set_input_files",
            "--session",
            sid,
            "--selector",
            selector,
            "--paths",
            &paths_json,
        ],
    )
}

fn set_input_files(
    harness: &DaemonTestHarness,
    sid: &str,
    selector: &str,
    paths: &[&str],
) -> serde_json::Value {
    let paths_json = serde_json::to_string(paths).unwrap();
    let out = run_loom(
        harness,
        &[
            "action",
            "web.set_input_files",
            "--session",
            sid,
            "--selector",
            selector,
            "--paths",
            &paths_json,
        ],
    );
    serde_json::from_str(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "set_input_files({selector}) stdout not JSON: {e}; status={} stderr={:?}",
            out.status, out.stderr
        )
    })
}

// ─── web-world provisioning (same as live_frame_targeting) ────────────────────

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
        // LOOM_TEST_VENDORED_WASM=1 → compile the COMMITTED vendored guest (the
        // shape the released binary loads), to isolate fresh-build vs vendored
        // behaviour differences. Default: the fresh `--release` guest.
        let use_vendored = std::env::var("LOOM_TEST_VENDORED_WASM").as_deref() == Ok("1");
        let wasm = if use_vendored {
            workspace_root().join("loom-cli/vendor/loom_surface_web.wasm")
        } else {
            workspace_root().join("target/wasm32-wasip2/release/loom_surface_web.wasm")
        };
        assert!(
            wasm.exists(),
            "build: cargo build --target wasm32-wasip2 -p loom-surface-web --release"
        );
        let suffix = if use_vendored { "vendored" } else { "release" };
        let cwasm = workspace_root().join(format!(
            "target/loom-web-verb-driving-cwasm/loom_surface_web-{suffix}.cwasm"
        ));
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
