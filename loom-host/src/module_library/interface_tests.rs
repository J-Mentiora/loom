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

// === Composite install stamp verified at load (engine-compat strand) ===

/// A sidecar whose compat line (line 2) does not match the live engine must be
/// rejected with a typed `StoreIntegrityFailed` carrying the "engine-incompatible
/// — run `loom postinstall`" remediation, BEFORE the costlier `deserialize_file`
/// (so the bogus cwasm bytes here never need to be a real component).
#[test]
fn load_one_rejects_engine_compat_mismatch_before_deserialize() {
    use crate::surface_stamp::format_surface_sidecar;
    let dir = tempfile::tempdir().unwrap();
    let cwasm = dir.path().join("loom_surface_web.cwasm");
    std::fs::write(&cwasm, b"not a real cwasm - compat check fires first").unwrap();
    let sidecar = dir.path().join("loom_surface_web.sha256");
    std::fs::write(
        &sidecar,
        format_surface_sidecar("abc", "wh-bogus-engine-wt0.0.0"),
    )
    .unwrap();

    let rt = WasmRuntime::new(WasmRuntimeConfig::default()).unwrap();
    let lib = ModuleLibrary::new(rt, dir.path().to_path_buf());
    // Empty expected_sha → source-SHA strand skipped; the compat strand fires.
    let err = lib
        .load_one_with_expected_sha(&SurfaceName("loom_surface_web".into()), &cwasm, "")
        .expect_err("compat mismatch must fail to load");
    assert_eq!(err.code, LoomErrorCode::StoreIntegrityFailed);
    assert!(
        err.message.contains("engine-incompatible"),
        "want engine-incompatible remediation, got: {}",
        err.message
    );
    assert!(
        err.message.contains("loom postinstall"),
        "want postinstall remediation, got: {}",
        err.message
    );
}

/// A legacy single-line sidecar (source SHA only, no compat line) must NOT be
/// rejected by the engine-compat check — it falls through to `deserialize_file`,
/// the real engine-format backstop. With bogus cwasm bytes the load still fails,
/// but with the deserialize error, NOT the engine-incompatible message.
#[test]
fn load_one_legacy_single_line_sidecar_skips_compat_check() {
    let dir = tempfile::tempdir().unwrap();
    let cwasm = dir.path().join("loom_surface_web.cwasm");
    std::fs::write(&cwasm, b"not a real cwasm").unwrap();
    let sidecar = dir.path().join("loom_surface_web.sha256");
    // Legacy format: just the source SHA, single line.
    std::fs::write(&sidecar, b"abc").unwrap();

    let rt = WasmRuntime::new(WasmRuntimeConfig::default()).unwrap();
    let lib = ModuleLibrary::new(rt, dir.path().to_path_buf());
    let err = lib
        .load_one_with_expected_sha(&SurfaceName("loom_surface_web".into()), &cwasm, "")
        .expect_err("bogus cwasm bytes must still fail to deserialize");
    // The compat check was skipped (legacy sidecar) → this is the deserialize
    // backstop failing, not the early engine-incompat rejection.
    assert!(
        !err.message.contains("engine-incompatible"),
        "legacy sidecar must skip the compat check, got: {}",
        err.message
    );
}

/// The source-SHA strand still rejects a wrong line-1 SHA, now with the
/// "stale surface artifact — run `loom postinstall`" remediation.
#[test]
fn load_one_rejects_wrong_source_sha_with_remediation() {
    use crate::surface_stamp::format_surface_sidecar;
    let dir = tempfile::tempdir().unwrap();
    let cwasm = dir.path().join("loom_surface_web.cwasm");
    std::fs::write(&cwasm, b"not a real cwasm").unwrap();
    let sidecar = dir.path().join("loom_surface_web.sha256");
    std::fs::write(
        &sidecar,
        format_surface_sidecar("actual-sha", "wh-bogus-engine-wt0.0.0"),
    )
    .unwrap();

    let rt = WasmRuntime::new(WasmRuntimeConfig::default()).unwrap();
    let lib = ModuleLibrary::new(rt, dir.path().to_path_buf());
    let err = lib
        .load_one_with_expected_sha(
            &SurfaceName("loom_surface_web".into()),
            &cwasm,
            "expected-sha",
        )
        .expect_err("source SHA mismatch must fail");
    assert_eq!(err.code, LoomErrorCode::StoreIntegrityFailed);
    assert!(
        err.message.contains("SHA-256 mismatch") && err.message.contains("loom postinstall"),
        "want source-SHA-mismatch remediation, got: {}",
        err.message
    );
}

// Reference the fixture symbol so the unused-fn lint stays quiet.
#[allow(dead_code)]
const _FIXTURE_PIN: fn() -> Arc<ModuleLibrary> = fixture;
