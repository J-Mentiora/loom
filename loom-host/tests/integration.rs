//! Integration tests — `loom-host` contract (AC-HOST-01 / IC-HOST-07 / IC-HOST-08).
//!
//! Contract: `projects/loom/architecture/contracts/loom-host_contract.md`
//!
//! Tests exercise the host stack without real WASM modules (pre-postinstall):
//! runtime construction, module library empty-dir behaviour, graceful
//! surface-unavailability, and HostConfig defaults.

use loom_core::error::LoomErrorCode;
use loom_host::module_library::{ModuleLibrary, SurfaceName};
use loom_host::wasm_host::HostConfig;
use loom_host::wasm_runtime::{WasmRuntime, WasmRuntimeConfig};
use loom_host::wit_type_marshaller::Mode;
use std::sync::Arc;
use tempfile::TempDir;

// ─── Test 1: Happy — WasmRuntime constructs successfully ─────────────────────

/// Contract: WasmRuntime::new with default config succeeds (AC-HOST-01).
/// The engine must be constructed and the runtime Arc'd within 2000 ms.
#[test]
fn test_wasm_runtime_new_default_succeeds() {
    let t0 = std::time::Instant::now();
    let runtime = WasmRuntime::new(WasmRuntimeConfig::default())
        .expect("WasmRuntime::new with default config must succeed");
    let elapsed = t0.elapsed();

    // Sanity: Arc is valid (access the inner via `Arc::strong_count`)
    assert!(
        Arc::strong_count(&runtime) >= 1,
        "runtime Arc must have at least 1 ref"
    );

    // SLA: wasmtime engine construction ≤ 2000ms (cold JIT)
    assert!(
        elapsed < std::time::Duration::from_millis(2000),
        "WasmRuntime::new SLA violated: {}ms > 2000ms",
        elapsed.as_millis()
    );
}

// ─── Test 2: Happy — ModuleLibrary::load_all on empty dir returns no failures ─

/// Contract: ModuleLibrary::load_all on an empty surfaces dir returns zero
/// failures and an empty library (IC-HOST-07).
#[test]
fn test_module_library_load_all_empty_dir_returns_no_failures() {
    let dir = TempDir::new().unwrap();
    let runtime = WasmRuntime::new(WasmRuntimeConfig::default()).expect("WasmRuntime must succeed");

    let library = ModuleLibrary::new(runtime, dir.path().to_path_buf());
    let failures = library
        .load_all()
        .expect("load_all on empty dir must return Ok");

    assert!(
        failures.is_empty(),
        "load_all on empty dir must return 0 failures; got {:?}",
        failures
    );
}

// ─── Test 3: Happy — ModuleLibrary::load_all on nonexistent dir is not an error

/// Contract: load_all with a nonexistent surfaces_dir returns Ok([]) (IC-HOST-07).
/// A missing dir means "postinstall not run yet" — not an error.
#[test]
fn test_module_library_load_all_nonexistent_dir_returns_empty() {
    let runtime = WasmRuntime::new(WasmRuntimeConfig::default()).expect("WasmRuntime must succeed");

    let library = ModuleLibrary::new(
        runtime,
        std::path::PathBuf::from("/nonexistent/dir/loom/surfaces"),
    );
    let failures = library
        .load_all()
        .expect("load_all on nonexistent dir must return Ok([]) not Err");

    assert!(
        failures.is_empty(),
        "nonexistent surfaces dir must yield 0 failures; got {:?}",
        failures
    );
}

// ─── Test 4: Error — get() on unloaded surface returns Unsupported ────────────

/// Contract: ModuleLibrary::get on a surface that was never loaded returns
/// LoomErrorCode::Unsupported — NEVER triggers compilation (IC-HOST-07).
#[test]
fn test_module_library_get_unloaded_surface_returns_unsupported() {
    let dir = TempDir::new().unwrap();
    let runtime = WasmRuntime::new(WasmRuntimeConfig::default()).expect("WasmRuntime must succeed");

    let library = ModuleLibrary::new(runtime, dir.path().to_path_buf());
    library.load_all().unwrap();

    let result = library.get(&SurfaceName("web".into()));
    assert!(result.is_err(), "get of unloaded surface must fail");
    let err = result.err().unwrap();

    assert_eq!(
        err.code,
        LoomErrorCode::Unsupported,
        "unloaded surface must return Unsupported; got {:?}",
        err.code
    );
}

// ─── Test 5: Happy — HostConfig::default has expected field values ────────────

/// Contract: HostConfig::default defaults are well-defined (BC §6, IC-HOST-08).
/// - redaction_enabled = true (data safety default)
/// - default_mode = Mode::Live (real execution default)
/// - surfaces_dir is non-empty string (not empty/blank)
#[test]
fn test_host_config_default_fields() {
    let cfg = HostConfig::default();

    assert!(
        cfg.redaction_enabled,
        "HostConfig::default must have redaction_enabled = true"
    );
    assert_eq!(
        cfg.default_mode,
        Mode::Live,
        "HostConfig::default must have default_mode = Live; got {:?}",
        cfg.default_mode
    );
    // surfaces_dir must point somewhere non-trivial
    let dir_str = cfg.surfaces_dir.to_string_lossy();
    assert!(
        !dir_str.is_empty() && dir_str.len() > 1,
        "HostConfig::default surfaces_dir must be a non-empty path; got {:?}",
        cfg.surfaces_dir
    );
}
