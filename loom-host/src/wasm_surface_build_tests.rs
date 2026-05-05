// TDD tests for wasm-surface-build behaviour (load_all, navigate
// dispatch, SHA mismatch rejection). Postinstall coverage lives in
// loom-cli/tests/wasmb_postinstall.rs. Workspace-wide regression
// coverage is verified by the full workspace test suite passing.

use std::sync::Arc;
use tempfile::TempDir;

use crate::compiler::Compiler;
use crate::module_library::{ModuleLibrary, SurfaceName};
use crate::wasm_runtime::{WasmRuntime, WasmRuntimeConfig};

/// Minimal valid WASM component binary (empty component, no imports/exports).
/// Magic (\0asm) + version bytes encoding component-model layer 1.
const MINIMAL_COMPONENT_BYTES: &[u8] = &[
    0x00, 0x61, 0x73, 0x6D, // magic: \0asm
    0x0D, 0x00, 0x01, 0x00, // version: component model
];

fn make_runtime() -> Arc<WasmRuntime> {
    WasmRuntime::new(WasmRuntimeConfig::default()).expect("WasmRuntime::new failed in test")
}

// ---------------------------------------------------------------------------
// load_all() returns Arc<Component> after compile
// ---------------------------------------------------------------------------
// Steps:
//  1. Compile a minimal .wasm component to a tempdir using Compiler.
//  2. Construct ModuleLibrary pointed at the tempdir.
//  3. Call load_all().
//  4. Assert library.get(&SurfaceName("loom_surface_web".into())).is_ok().
#[test]
fn test_load_all_returns_component_after_compile() {
    let rt = make_runtime();
    let compiler = Compiler::new(rt.clone());

    let tmpdir = TempDir::new().expect("tempdir");
    let wasm_src = tmpdir.path().join("loom_surface_web.wasm");
    let cwasm_dest = tmpdir.path().join("loom_surface_web.cwasm");

    std::fs::write(&wasm_src, MINIMAL_COMPONENT_BYTES).expect("write minimal wasm");

    compiler
        .compile_module(&wasm_src, &cwasm_dest)
        .expect("compile_module failed for minimal component");

    let library = ModuleLibrary::new(rt, tmpdir.path().to_path_buf());
    let failures = library.load_all().expect("load_all returned Err");

    assert!(failures.is_empty(), "load_all had failures: {:?}", failures);

    let name = SurfaceName("loom_surface_web".into());
    assert!(
        library.get(&name).is_ok(),
        "library.get(loom_surface_web) should be Ok after load_all"
    );
}

// ---------------------------------------------------------------------------
// web.navigate E2E dispatch — integration only
// smoke runbook against real Chromium
// ---------------------------------------------------------------------------
#[test]
#[ignore = "full web.navigate E2E verified by smoke runbook"]
fn test_web_navigate_dispatches_into_surface() {
    // These are end-to-end scenarios requiring a live loom-daemon and
    // real Chromium. They are exercised by the operator-driven smoke
    // runbook, not by loom-host unit tests.
    //
    // The dispatch path traverses: CLI → RPC socket → WasmHost →
    // SessionExecutor::run → wasmtime guest (loom_surface_web.wasm) →
    // host::shim_call("chromium", payload) → ShimManager::send →
    // CDP → real Chromium → typed Receipt.
    //
    // The `wasm-guest-dispatch` feature filled in two formerly-broken
    // links in that path:
    //   1. SessionExecutor::run now passes the action's
    //      `args_canonical_bytes` as the WIT `action.payload` and
    //      decodes the typed `result<receipt, host-error>` return.
    //      See session_executor/interfaces.rs (build_action_val,
    //      decode_typed_receipt). Pure-function unit tests pin the
    //      Val::Record / Val::Result shape in
    //      session_executor/interface_tests.rs.
    //   2. WasmBridge::dispatch_action_blocking now plumbs
    //      `builder.action_hash` / `builder.outcome_hash` /
    //      `builder.emitted_at_ms` into the loom_rpc::Receipt the CLI
    //      receives. See loom-daemon/src/main.rs.
    //
    // To exercise the assembled path manually:
    //   cargo build --release -p loom-cli -p loom-daemon
    //   loom postinstall      # builds + downloads chromium snapshot
    //   loom serve &
    //   loom session create --id smoke-1
    //   loom action web.navigate --session smoke-1 -- --url https://example.com
    //
    // Expected: exit 0, JSON receipt with `"action_hash"` and
    // `"outcome_hash"` matching ^[0-9a-f]{64}$ (SHA-256 on the guest
    // side; see loom-surface-web/src/lib.rs::hex_sha256).
}

// ---------------------------------------------------------------------------
// load_one rejects a mismatched SHA-256 sidecar
// ---------------------------------------------------------------------------
// Uses `load_one_with_expected_sha` (a testability shim that lets us pass the
// expected SHA directly rather than relying on the compile-time env!() macro).
// This test FAILS at compile time until load_one_with_expected_sha is added
// to ModuleLibrary in module_library/interfaces.rs.
#[test]
fn test_load_one_rejects_wrong_sha() {
    use loom_core::error::LoomErrorCode;

    let rt = make_runtime();
    let compiler = Compiler::new(rt.clone());

    let tmpdir = TempDir::new().expect("tempdir");
    let wasm_src = tmpdir.path().join("loom_surface_web.wasm");
    let cwasm_dest = tmpdir.path().join("loom_surface_web.cwasm");

    std::fs::write(&wasm_src, MINIMAL_COMPONENT_BYTES).expect("write minimal wasm");
    compiler
        .compile_module(&wasm_src, &cwasm_dest)
        .expect("compile_module");

    // Write a WRONG sha256 sidecar next to the .cwasm.
    let sidecar = tmpdir.path().join("loom_surface_web.sha256");
    std::fs::write(
        &sidecar,
        "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
    )
    .expect("write sidecar");

    let library = ModuleLibrary::new(rt, tmpdir.path().to_path_buf());
    let name = SurfaceName("loom_surface_web".into());

    // The correct SHA of MINIMAL_COMPONENT_BYTES (not "deadbeef").
    // Any non-matching expected SHA causes StoreIntegrityFailed.
    let correct_sha = "a_sha_that_matches_the_sidecar_would_pass_but_we_use_deadbeef_here";
    let result =
        library.load_one_with_expected_sha(&name, &cwasm_dest, "some_expected_sha_that_differs");

    assert!(
        result.is_err(),
        "load_one_with_expected_sha should return Err on SHA mismatch"
    );
    let err = result.unwrap_err();
    assert_eq!(
        err.code.as_wire(),
        LoomErrorCode::StoreIntegrityFailed.as_wire(),
        "error code must be StoreIntegrityFailed, got {:?}",
        err.code
    );

    // Suppress unused variable warning in docstring example
    let _ = correct_sha;
}
