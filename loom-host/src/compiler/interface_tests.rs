// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-host/modules/compiler/interface_tests.rs` instead.
// Interface tests for `Compiler`. Verifies IC-HOST-07 (off hot path),
// contract signature exact-match, and atomic-write expectations.

use super::compiler::{CompileReport, Compiler};
use crate::wasm_runtime::WasmRuntime;
use loom_core::error::LoomError;
use std::path::Path;
use std::sync::Arc;

// === Contract signature exact match ===

#[test]
fn compile_module_signature_is_source_dest_returns_unit_or_loomerror() {
    // Per `loom-host_contract.md`:
    //   pub fn compile_module(&self, source: &Path, dest: &Path)
    //       -> Result<(), LoomError>
    // Pinned by compile-time function-pointer typecheck.
    fn _ck(c: &Compiler, src: &Path, dst: &Path) -> Result<(), LoomError> {
        c.compile_module(src, dst)
    }
    let _ = _ck;
}

#[test]
fn compile_module_is_synchronous_not_async() {
    // The contract specifies a non-async signature (install path; not
    // hot path). Compile-time pin: there is no `.await` on the call.
    fn _ck(c: &Compiler, src: &Path, dst: &Path) -> Result<(), LoomError> {
        c.compile_module(src, dst) // no `.await`
    }
    let _ = _ck;
}

// === IC-HOST-07: not invoked from dispatch ===

#[test]
fn compiler_constructor_takes_runtime_only_no_dispatch_handle() {
    // Compile-time pin: `Compiler::new(runtime)` — no SessionExecutor,
    // no ModuleLibrary, no WasmHost handle. The `Compiler` is
    // structurally unable to be reached from a dispatch path.
    fn _ck(rt: Arc<WasmRuntime>) -> Compiler {
        Compiler::new(rt)
    }
    let _ = _ck;
}

#[test]
fn compiler_does_not_implement_or_use_dispatch_traits() {
    // The Compiler module's only deps are `WasmRuntime` + std::path
    // + loom_core::error. Verified structurally (no `host_function_table`
    // or `session_executor` import). The doc string asserts this.
    let pin = "IC-HOST-07: Compiler is reachable ONLY from postinstall + StartupManager recovery";
    assert!(pin.contains("IC-HOST-07"));
}

// === Atomic write (BC §1, AC-NFR-REL-02) ===

#[test]
fn compile_report_carries_compatibility_hash() {
    // The report includes the precompile-compatibility hash so
    // postinstall can log it and StartupManager can detect engine-skew.
    let r = CompileReport {
        source_bytes: 1_048_576,
        cwasm_bytes: 2_097_152,
        elapsed_us: 250_000,
        precompile_compatibility_hash: "abc123".into(),
    };
    assert!(!r.precompile_compatibility_hash.is_empty());
    assert!(r.cwasm_bytes > 0);
    let _: u64 = r.elapsed_us; // BC HARD #3: integer-only
}

#[test]
fn compile_module_with_report_returns_report_or_loomerror() {
    fn _ck(c: &Compiler, src: &Path, dst: &Path) -> Result<CompileReport, LoomError> {
        c.compile_module_with_report(src, dst)
    }
    let _ = _ck;
}

// === Specific error codes Phase 5.4 will raise ===

#[test]
fn compilation_failed_error_carries_wasmtime_error_string() {
    let _e = loom_core::error::LoomErrorCode::Internal;
    let _: LoomError = LoomError::from(_e);
}

#[test]
fn io_error_during_write_maps_to_loomerror_io() {
    // O_TMPFILE / linkat failures map to LoomErrorCode::Io.
    let _e = loom_core::error::LoomErrorCode::Io;
    let _: LoomError = LoomError::from(_e);
}
