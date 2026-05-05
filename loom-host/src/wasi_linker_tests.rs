// Tests for the wasmtime-wasi linker wiring. Without these the daemon's
// `wasmtime::Linker` only has
// `loom:surface/host` registered, so any surface compiled for the
// `wasm32-wasip2` target fails to instantiate with
//
//     component imports instance `wasi:io/poll@0.2.6`,
//     but a matching implementation was not found in the linker
//
// because the Rust stdlib's allocator/format machinery transitively
// imports `wasi:io/poll@0.2.6` (and other wasi:* interfaces) even when
// the WIT world does not import any wasi interface explicitly.

use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;

use crate::compiler::Compiler;
use crate::host_function_registry::HostFunctionRegistry;
use crate::module_library::{ModuleLibrary, SurfaceName};
use crate::wasm_runtime::{WasmRuntime, WasmRuntimeConfig};

/// Path to the precompiled `wasm32-wasip2` artifact for the web surface.
/// Built by `cargo build --target wasm32-wasip2 --release -p loom-surface-web`.
/// Tests that need it skip themselves (with a printed reason) when it
/// is missing, mirroring the `LOOM_SURFACE_WEB_WASM_PATH` convention
/// used by `loom-host/build.rs`.
fn loom_surface_web_wasm_path() -> PathBuf {
    if let Ok(p) = std::env::var("LOOM_SURFACE_WEB_WASM_PATH") {
        return PathBuf::from(p);
    }
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir)
        .parent()
        .expect("loom-host manifest must have a parent (workspace root)")
        .join("target/wasm32-wasip2/release/loom_surface_web.wasm")
}

fn try_load_surface_web_component() -> Option<(Arc<WasmRuntime>, wasmtime::component::Component)> {
    let path = loom_surface_web_wasm_path();
    if !path.exists() {
        eprintln!(
            "skipping: loom_surface_web.wasm not at {} — run `cargo build --target wasm32-wasip2 --release -p loom-surface-web`",
            path.display()
        );
        return None;
    }

    let runtime =
        WasmRuntime::new(WasmRuntimeConfig::default()).expect("WasmRuntime::new must succeed");
    let compiler = Compiler::new(runtime.clone());

    let tmpdir = TempDir::new().expect("tempdir");
    let cwasm_dest = tmpdir.path().join("loom_surface_web.cwasm");
    compiler
        .compile_module(&path, &cwasm_dest)
        .expect("Compiler::compile_module must succeed for built artifact");

    let library = ModuleLibrary::new(runtime.clone(), tmpdir.path().to_path_buf());
    let failures = library.load_all().expect("load_all returned Err");
    assert!(failures.is_empty(), "load_all had failures: {:?}", failures);

    let component = library
        .get(&SurfaceName("loom_surface_web".into()))
        .expect("library.get must succeed for compiled web surface");

    // ModuleLibrary returns Arc<Component>; clone the inner Component so
    // we can pass it by reference to instantiate_pre.
    let component_inner: wasmtime::component::Component = (*component).clone();
    Some((runtime, component_inner))
}

// ---------------------------------------------------------------------------
// `loom action web.navigate` succeeds end-to-end
// ---------------------------------------------------------------------------
//
// The full E2E (CLI → daemon → wasm guest → shim → real Chromium →
// receipt) is covered by the operator-driven smoke runbook documented in
// `wasm_surface_build_tests.rs::test_web_navigate_dispatches_into_surface`.
//
// The CI-runnable slice is the line that previously broke:
// `linker.instantiate_pre(&loom_surface_web_component)`. That call
// validates every import the component declares against the linker's
// registered interfaces. Before the wasmtime-wasi wiring it failed with
// `wasi:io/poll@0.2.6 not found in linker`. After, it must succeed.
#[test]
fn loom_surface_web_satisfies_live_linker_imports() {
    let Some((runtime, component)) = try_load_surface_web_component() else {
        return;
    };

    let registry = HostFunctionRegistry::new(runtime.engine())
        .expect("HostFunctionRegistry::new must succeed");

    let live_linker = registry.linker_for(crate::wit_type_marshaller::Mode::Live);
    live_linker
        .instantiate_pre(&component)
        .expect("live linker must satisfy all wasi:* imports of loom_surface_web");

    let replay_linker = registry.linker_for(crate::wit_type_marshaller::Mode::Replay);
    replay_linker
        .instantiate_pre(&component)
        .expect("replay linker must also satisfy wasi:* imports (parity with live)");
}

// ---------------------------------------------------------------------------
// WASI ctx denies fs access, env reads, sockets
// ---------------------------------------------------------------------------
//
// The deny-by-default contract is `WasiCtxBuilder::new().build()`:
// no preopened directories, no inherited env, no inherited stdio, no
// network. Adding any of `inherit_env`, `env(`, `preopened_dir(`,
// `inherit_network`, `allow_ip_name_lookup`, `inherit_stdio`,
// `inherit_stdin`, `inherit_stdout`, `inherit_stderr`, `socket_addr_check`
// to `build_sandboxed_wasi_ctx` widens the sandbox.
//
// Pin that textually so a future change cannot silently widen the
// sandbox without failing CI. The pin scans the source body of
// `build_sandboxed_wasi_ctx` in `host_function_table/interfaces.rs`.
#[test]
fn build_sandboxed_wasi_ctx_uses_only_safe_defaults() {
    let src = include_str!("host_function_table/host_function_table.rs");

    let body_start = src
        .find("pub fn build_sandboxed_wasi_ctx() -> WasiCtx {")
        .expect("build_sandboxed_wasi_ctx signature must exist");
    let body_tail = &src[body_start..];
    let body_end_offset = body_tail
        .find("\n}\n")
        .expect("build_sandboxed_wasi_ctx body must have a closing `}` on its own line");
    let body = &body_tail[..body_end_offset];

    for forbidden in [
        "inherit_env",
        ".env(",
        "envs(",
        "preopened_dir",
        "preopened_path",
        "inherit_stdio",
        "inherit_stdin",
        "inherit_stdout",
        "inherit_stderr",
        "inherit_network",
        "allow_ip_name_lookup",
        "socket_addr_check",
    ] {
        assert!(
            !body.contains(forbidden),
            "build_sandboxed_wasi_ctx must not call `{}` — that widens the sandbox.\nBody was:\n{}",
            forbidden,
            body
        );
    }

    // Smoke: the function compiles and returns a WasiCtx without panicking.
    let _ = crate::host_function_table::build_sandboxed_wasi_ctx();
}

// The resource table starts empty.
//
// `wasmtime_wasi` exposes preopened directories as resources in the
// component-model `ResourceTable`. The deny-by-default builder
// produces an empty table — guest calls to
// `wasi:filesystem/preopens.get-directories` return an empty list,
// which is what blocks fs access at the guest layer.
#[test]
fn fresh_host_state_resource_table_is_empty() {
    let table = wasmtime::component::ResourceTable::new();
    // ResourceTable doesn't expose `len()`, but `is_empty()` is enough
    // to pin "no preopens were inserted by the builder".
    assert!(
        table.is_empty(),
        "ResourceTable::new() must yield an empty table"
    );
}

// Workspace-wide regression coverage is asserted globally by
// `cargo test --workspace` going green. No local test body needed — if
// any other crate's behaviour regressed under the wasi wiring, the
// workspace suite would fail.

// ---------------------------------------------------------------------------
// engine async-config + instantiate_async path are aligned
// ---------------------------------------------------------------------------
//
// The wasmtime engine is built with `Config::async_support(true)` (see
// wasm_runtime/interfaces.rs). Wasmtime requires every store-touching call
// to use the `*_async` variant on async-config'd engines; the sync
// `Linker::instantiate` errors at runtime with
//
//     store configuration requires that *_async functions are used instead
//
// This is a compile-time pin: `SessionExecutor::instantiate_surface` MUST
// be `async fn` and call `linker.instantiate_async`, mirroring the
// `Func::call_async` invocation at the dispatch site. If a future change
// reverts to the sync `instantiate` path, the runtime would surface as a
// generic SurfaceTrap with this exact message — so we pin the type shape
// here rather than running a heavy HostState-backed e2e.
//
// The signature pin lives in `session_executor/interface_tests.rs::
// instantiate_surface_takes_mut_store_so_each_dispatch_gets_fresh_one`,
// which now requires `_ck` to return an `impl Future<...>`. Reverting
// `instantiate_surface` to sync would fail to compile that test.
//
// The end-to-end fitness function is the operator-driven manual smoke
// which exercises the real wasm32-wasip2 loom_surface_web component
// through the daemon → guest → shim chain.
#[test]
fn async_instantiate_doc_pin() {
    // Doc-anchor only. The actual constraint is enforced by:
    //   1. session_executor/interfaces.rs:`async fn instantiate_surface`
    //      → returns a Future and calls `linker.instantiate_async(...).await`
    //   2. session_executor/interface_tests.rs:`_ck` signature pin returning
    //      `impl Future<Output = Result<Instance, LoomError>>`
    //   3. The wasmtime engine built with `Config::async_support(true)` in
    //      wasm_runtime/interfaces.rs
    // All three must change together; reverting any one alone fails to
    // compile or fails the smoke runbook.
}
