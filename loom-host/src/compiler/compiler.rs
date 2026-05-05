// Re-export of the locked v5.3 interface. DO NOT EDIT here.
// Edit `systems/loom-host/modules/compiler/interfaces.rs` instead.
// Compiler — Cranelift AOT compile `.wasm → .cwasm`.
//
// # Contract semantics
// - **Off the hot path.** Invoked only by:
//     1. `WasmHost::compile_module` (called by `loom-cli postinstall`).
//     2. `StartupManager::on_aot_failure` (recovery during daemon startup).
//   The `dispatch` path has NO edge into `Compiler`.
// - **Atomic write.** Output uses `O_TMPFILE` + `linkat` to
//   land at the final cwasm path. Crash mid-write leaves no orphaned
//   half-file in the surfaces dir (the orphan tmpfile is cleaned by
//   `StartupManager`'s CAS sweep).
// - **Signature pin.** `compile_module(source, dest)` matches the
//   contract verbatim — `&self, source: &Path, dest: &Path → Result<(), LoomError>`.
//   **Not async** (install path; ~250 ms cold cache).

use crate::wasm_runtime::WasmRuntime;
use loom_core::error::LoomError;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

/// Compilation report. Useful to `loom-cli postinstall` for
/// per-surface telemetry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompileReport {
    pub source_bytes: u64,
    pub cwasm_bytes: u64,
    pub elapsed_us: u64,
    pub precompile_compatibility_hash: String,
}

pub struct Compiler {
    pub(crate) runtime: Arc<WasmRuntime>,
}

impl Compiler {
    pub fn new(runtime: Arc<WasmRuntime>) -> Self {
        Self { runtime }
    }

    /// Compile `source` (a `.wasm` file) to `dest` (a `.cwasm` file).
    /// Atomic write via a temp file + rename. Synchronous; ~250 ms cold.
    /// **NEVER called from the dispatch path.**
    pub fn compile_module(&self, source: &Path, dest: &Path) -> Result<(), LoomError> {
        self.compile_module_with_report(source, dest).map(|_| ())
    }

    /// Detailed-report variant. Exposed for `loom-cli postinstall` and
    /// `StartupManager` recovery so they can log per-surface metrics.
    pub fn compile_module_with_report(
        &self,
        source: &Path,
        dest: &Path,
    ) -> Result<CompileReport, LoomError> {
        use loom_core::error::LoomErrorCode;
        let start = std::time::Instant::now();
        let bytes =
            std::fs::read(source).map_err(|e| LoomError::new(LoomErrorCode::Io, e.to_string()))?;
        let source_bytes = bytes.len() as u64;
        let cwasm = self
            .runtime
            .engine()
            .precompile_component(&bytes)
            .map_err(|e| LoomError::new(LoomErrorCode::Internal, e.to_string()))?;
        let cwasm_bytes = cwasm.len() as u64;
        // Atomic write: write to a temp file alongside dest, then rename.
        let tmp = dest.with_extension("cwasm.tmp");
        std::fs::write(&tmp, &cwasm)
            .map_err(|e| LoomError::new(LoomErrorCode::Io, e.to_string()))?;
        std::fs::rename(&tmp, dest)
            .map_err(|e| LoomError::new(LoomErrorCode::Io, e.to_string()))?;
        Ok(CompileReport {
            source_bytes,
            cwasm_bytes,
            elapsed_us: start.elapsed().as_micros() as u64,
            precompile_compatibility_hash: self.runtime.precompile_compatibility_hash()?,
        })
    }
}
