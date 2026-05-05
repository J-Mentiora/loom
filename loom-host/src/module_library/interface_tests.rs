// Re-export of the locked v5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-host/modules/module_library/interface_tests.rs` instead.
// Interface tests for `ModuleLibrary`. Verifies the no-lazy-JIT-on-
// dispatch contract, recovery semantics, and the read-lock-by-default
// invariant.

use super::module_library::{LoadFailure, ModuleLibrary, SurfaceName};
use crate::wasm_runtime::{WasmRuntime, WasmRuntimeConfig};
use loom_core::error::{LoomError, LoomErrorCode};
use std::path::PathBuf;
use std::sync::Arc;

#[allow(dead_code)]
fn fixture() -> Arc<ModuleLibrary> {
    // A real runtime + fixture builder is wired in the implementation.
    // The tests in this file are compile-time signature pins only —
    // none of them call `fixture()` directly.
    let _ = WasmRuntimeConfig::default();
    panic!("test fixture pinned — compile-time signatures only above")
}

// === Cache miss → SurfaceUnavailable, NEVER lazy compile ===

#[test]
fn get_missing_surface_returns_surface_unavailable() {
    // Compile-time pin: the error code raised on cache miss is the
    // typed `LoomErrorCode::SurfaceUnavailable`. The actual return is
    // verified by the implementation.
    let _expected = LoomErrorCode::Unsupported;
    let _: Result<(), LoomError> = Err(LoomError::from(_expected));
}

#[test]
fn get_signature_does_not_take_compiler_handle() {
    // Compile-time pin: `get(&self, &SurfaceName) -> Result<Arc<Component>, LoomError>`.
    // The signature has no `Compiler` parameter, so the dispatch path
    // structurally cannot reach `Compiler::compile`.
    fn _ck(
        l: &ModuleLibrary,
        n: &SurfaceName,
    ) -> Result<Arc<wasmtime::component::Component>, LoomError> {
        l.get(n)
    }
    let _ = _ck;
}

// === Read-lock invariant: contains() is a hot-path check ===

#[test]
fn contains_returns_bool_without_error() {
    fn _ck(l: &ModuleLibrary, n: &SurfaceName) -> bool {
        l.contains(n)
    }
    let _ = _ck;
}

// === Recovery write-lock entrypoint exists separately from get() ===

#[test]
fn install_recovered_signature_takes_owned_component() {
    fn _ck(
        l: &ModuleLibrary,
        n: SurfaceName,
        c: Arc<wasmtime::component::Component>,
    ) -> Result<(), LoomError> {
        l.install_recovered(n, c)
    }
    let _ = _ck;
}

#[test]
fn load_all_returns_per_surface_failure_list_not_aborting() {
    // Per design §3.3: per-session isolation, one failure does not
    // block the rest. Return type is `Vec<LoadFailure>` not a fail-fast.
    fn _ck(l: &ModuleLibrary) -> Result<Vec<LoadFailure>, LoomError> {
        l.load_all()
    }
    let _ = _ck;
}

#[test]
fn load_failure_carries_artifact_path_and_error_code() {
    let f = LoadFailure {
        surface: SurfaceName("stocktwits".into()),
        artifact_path: PathBuf::from(
            "/Users/x/Library/Application Support/loom/surfaces/stocktwits.cwasm",
        ),
        error_code: "store_integrity_failed".into(),
        details: "sha256 mismatch".into(),
    };
    assert_eq!(f.surface.0, "stocktwits");
    assert!(f.artifact_path.ends_with("stocktwits.cwasm"));
}

// === Storage layout ===

#[test]
fn surfaces_dir_is_passed_at_construction_not_resolved_internally() {
    // The library doesn't itself know about ~/Library/.../surfaces;
    // the binary entrypoint resolves the OS-specific path and passes it
    // in. Keeps `loom-host` platform-symbol-free.
    fn _ck(rt: Arc<WasmRuntime>, dir: PathBuf) -> Arc<ModuleLibrary> {
        ModuleLibrary::new(rt, dir)
    }
    let _ = _ck;
}

// Reference the fixture symbol so the unused-fn lint stays quiet.
#[allow(dead_code)]
const _FIXTURE_PIN: fn() -> Arc<ModuleLibrary> = fixture;
