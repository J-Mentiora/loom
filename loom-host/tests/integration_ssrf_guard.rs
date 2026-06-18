//! F40 SSRF — INTEGRATION coverage that the guard is actually WIRED into the
//! live host-function call path, not merely correct in isolation.
//!
//! The pure `validate_outbound_url` / `ip_is_blocked` predicates are unit-tested
//! in `host_impl.rs::ssrf_guard_tests`. A passing pure test does NOT prove the
//! guard is invoked: if a refactor dropped the `resolve_and_validate(...)?` call
//! out of `do_http_request` (or `do_http_request` out of `net_request`), those
//! unit tests would still be green while a compromised guest could reach
//! internal targets.
//!
//! These tests drive the REAL host function — `<HostState as Host>::net_request`
//! — exactly the way a WASM surface reaches the daemon's network position. The
//! HostState is built with the same wiring `make_host_state` uses in
//! `wasm_preemption_tests` (real `CoreApiFacade`, real determinism harness,
//! real `ShimManager`). The future returned by `net_request` runs
//! `resolve_and_validate` → `validate_outbound_url` for real.
//!
//! Failure mode if the guard were un-wired: `block_…_before_connect` would see
//! the server `accept()` a connection (block did NOT fire) and/or get a non
//! `url_blocked` error — the test would fail. That is the whole point.

use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use loom_core::core_api_facade::{CoreApiFacade, CoreConfig};
use loom_core::manifest_writer::SessionId;
use loom_host::host_function_table::{build_sandboxed_wasi_ctx, HostState};
use loom_host::host_observability::HostObservability;
use loom_host::shim_manager::ShimManager;
use loom_host::wit_type_marshaller::loom_surface_bindings::loom::surface::host::Host;
use loom_host::wit_type_marshaller::loom_surface_bindings::loom::surface::types::{
    HostError, NetReq,
};
use loom_host::wit_type_marshaller::Mode;
use loom_shared::types::{EpochMs, Seed};
use tempfile::TempDir;

/// Holds the temp dir alive for the duration of a test alongside the real core.
struct Fixture {
    core: Arc<CoreApiFacade>,
    obs: Arc<HostObservability>,
    _dir: TempDir,
}

fn fixture() -> Fixture {
    let dir = TempDir::new().expect("tempdir");
    let core = CoreApiFacade::new(
        CoreConfig {
            data_root: dir.path().join("data"),
            log_path: dir.path().join("daemon.log"),
            otel_enabled: false,
            default_seed: 42,
            checkpoint_every_n: 100,
        },
        Arc::new(loom_keychain::StubKeychain),
    )
    .expect("CoreApiFacade::new");
    let obs = HostObservability::new(true);
    Fixture {
        core,
        obs,
        _dir: dir,
    }
}

/// Build a real `HostState` in Live mode — the same struct
/// `SessionExecutor::run` flows through `wasmtime::Store`. Mirrors
/// `wasm_preemption_tests::make_host_state`.
fn live_host_state(fx: &Fixture) -> HostState {
    let determinism = fx.core.determinism();
    let tape_writer = determinism.new_tape_writer();
    HostState {
        core: fx.core.clone(),
        determinism,
        budget: fx.core.budget_enforcer(),
        session_id: SessionId("01SSRFTEST".into()),
        action_id: 1,
        mode: Mode::Live,
        receipt_builder: Default::default(),
        side_effects: Default::default(),
        host_call_metrics: Vec::new(),
        shim_manager: ShimManager::new(fx.obs.clone()),
        obs: fx.obs.clone(),
        tape_writer,
        replay_table: None,
        wasi_ctx: build_sandboxed_wasi_ctx(),
        wasi_table: wasmtime::component::ResourceTable::new(),
        seed: Seed(42),
        epoch_ms: EpochMs(0),
        no_blocklist: false,
        no_determinism: false,
        profile: "safe".into(),
        downloads_dir: None,
        capture_dom_after: false,
    }
}

fn get_req(url: &str) -> NetReq {
    NetReq {
        method: "GET".into(),
        url: url.into(),
        headers: vec![],
        body: vec![],
    }
}

/// PRIMARY F40 property: `net_request` aimed at a loopback target is REFUSED
/// by the SSRF guard, and the refusal happens BEFORE any TCP connect. We bind a
/// real listener and assert it never accepts — the guard must short-circuit in
/// `resolve_and_validate` ahead of reqwest's connect. If the guard were dropped
/// from `do_http_request`, reqwest would connect to our listener (accept fires)
/// and/or the error would not be a typed `url_blocked` verdict.
#[tokio::test]
async fn net_request_blocks_loopback_before_connect() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
    let port = listener.local_addr().unwrap().port();
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");

    let fx = fixture();
    let mut state = live_host_state(&fx);

    let url = format!("http://127.0.0.1:{port}/internal");
    let result = state.net_request(get_req(&url)).await;

    let err = result.expect_err("net_request to loopback must be REFUSED by the SSRF guard");
    let HostError::Internal(msg) = err else {
        panic!("expected HostError::Internal carrying the url_blocked verdict, got: {err:?}");
    };
    assert!(
        msg.contains("url_blocked"),
        "refusal must be the typed SSRF verdict, got: {msg}"
    );
    assert!(
        msg.contains("private_or_loopback_ip"),
        "verdict must name the loopback reason, got: {msg}"
    );

    // Block-before-connect: the listener must NOT have accepted anything. Give
    // any (erroneous) in-flight connect a brief window to land, then assert the
    // accept queue is empty.
    tokio::time::sleep(Duration::from_millis(150)).await;
    match listener.accept() {
        Ok((_stream, peer)) => panic!(
            "SSRF guard did NOT fire before connect — listener accepted a connection from {peer}; \
             the guard is not wired into do_http_request"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => { /* good: no connection */ }
        Err(e) => panic!("unexpected accept error: {e}"),
    }
}

/// The cloud-metadata endpoint (169.254.169.254) — the canonical SSRF target —
/// is refused through the real path with a link-local verdict.
#[tokio::test]
async fn net_request_blocks_cloud_metadata_endpoint() {
    let fx = fixture();
    let mut state = live_host_state(&fx);

    let result = state
        .net_request(get_req("http://169.254.169.254/latest/meta-data/"))
        .await;
    let err = result.expect_err("cloud-metadata endpoint must be refused");
    let HostError::Internal(msg) = err else {
        panic!("expected HostError::Internal, got: {err:?}");
    };
    assert!(msg.contains("url_blocked"), "got: {msg}");
    assert!(
        msg.contains("private_or_loopback_ip") && msg.contains("169.254.169.254"),
        "verdict must name the metadata IP, got: {msg}"
    );
}

/// A private RFC1918 literal (10.0.0.0/8) is refused through the real path —
/// covers the `ipv4_is_blocked` private-range branch on the live route
/// (distinct from the loopback range covered above). Uses a literal IP so the
/// request reaches `validate_outbound_url` via the no-DNS path.
#[tokio::test]
async fn net_request_blocks_private_rfc1918() {
    let fx = fixture();
    let mut state = live_host_state(&fx);

    let result = state.net_request(get_req("http://10.1.2.3/admin")).await;
    let err = result.expect_err("RFC1918 private address must be refused");
    let HostError::Internal(msg) = err else {
        panic!("expected HostError::Internal, got: {err:?}");
    };
    assert!(
        msg.contains("url_blocked") && msg.contains("private_or_loopback_ip"),
        "got: {msg}"
    );
    assert!(
        msg.contains("10.1.2.3"),
        "verdict must name the IP, got: {msg}"
    );
}

/// A non-http(s) scheme is refused through the real path with the scheme verdict
/// — proves the scheme allowlist runs on the wired route. We use an
/// `ftp://<literal-ip>` URL so resolution succeeds (literal IP, no DNS) and the
/// request reaches the scheme allowlist inside `validate_outbound_url` rather
/// than short-circuiting earlier on a missing host.
#[tokio::test]
async fn net_request_blocks_non_http_scheme() {
    let fx = fixture();
    let mut state = live_host_state(&fx);

    let result = state.net_request(get_req("ftp://8.8.8.8/file")).await;
    let err = result.expect_err("ftp:// scheme must be refused");
    let HostError::Internal(msg) = err else {
        panic!("expected HostError::Internal, got: {err:?}");
    };
    assert!(
        msg.contains("url_blocked") && msg.contains("scheme_not_allowed"),
        "got: {msg}"
    );
}

/// CONTROL: a public-shaped destination PASSES the guard. We aim at a public IP
/// on a port that will not establish a session in CI; the request leaves the
/// guard (no `url_blocked` verdict) and fails — if at all — at the transport
/// layer. The discriminating property is the ABSENCE of a `url_blocked` verdict:
/// the guard returned Ok for a public address, so a guard that over-blocked
/// (e.g. treated every address as private) would make this test fail.
#[tokio::test]
async fn net_request_allows_public_shaped_address_past_guard() {
    let fx = fixture();
    let mut state = live_host_state(&fx);

    // 8.8.8.8:9 (discard) — public per `ip_is_blocked`; a connect here is
    // filtered/dropped in sandboxed CI, so bound the wait. The guard runs
    // synchronously BEFORE the connect, so a `url_blocked` verdict (if it
    // over-blocked) returns near-instantly — 3s comfortably distinguishes
    // "blocked fast" from "passed the guard, now connecting".
    let fut = state.net_request(get_req("http://8.8.8.8:9/"));
    let outcome = tokio::time::timeout(Duration::from_secs(3), fut).await;

    match outcome {
        Err(_elapsed) => {
            // Connect hung past the bound: the guard let it through to the
            // transport (it would have returned a url_blocked verdict
            // synchronously and fast had it blocked). That satisfies the
            // property under test.
        }
        Ok(Ok(_resp)) => { /* unlikely in CI, but a real public response also means the guard passed */
        }
        Ok(Err(HostError::Internal(msg))) => {
            assert!(
                !msg.contains("url_blocked"),
                "public-shaped address must PASS the SSRF guard, but it was blocked: {msg}"
            );
        }
        Ok(Err(other)) => {
            // Any non-Internal error is necessarily not a url_blocked verdict
            // (those are emitted as Internal), so the guard passed.
            let _ = other;
        }
    }
}
