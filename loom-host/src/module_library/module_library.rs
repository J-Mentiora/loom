// ModuleLibrary — preloaded `Arc<wasmtime::component::Component>` per
// surface. Populated at `WasmHost::new`; never mutated on the dispatch
// hot path.
//
// # Contract semantics
// - **Cache miss is NOT a lazy compile.** Dispatch to a missing surface
//   returns `LoomErrorCode::SurfaceUnavailable` — `Compiler::compile` is
//   NEVER invoked from `dispatch`.
// - **`RwLock` for recovery rebuild ONLY.** The dispatch path
//   `get(name)` takes the read lock and returns `Arc<Component>` clone.
//   The write lock is taken only by `StartupManager` recovery when a
//   corrupted artifact must be re-compiled and reloaded.
// - **Cwasm artifact path includes engine hash.** Built by
//   `Compiler` from `WasmRuntime::precompile_compatibility_hash()` so a
//   wasmtime upgrade naturally invalidates artifacts.
// - **On-disk storage layout:**
//     macOS: `~/Library/Application Support/loom/surfaces/<name>.cwasm`
//     Linux: `$XDG_DATA_HOME/loom/surfaces/<name>.cwasm`

use crate::wasm_runtime::WasmRuntime;
use loom_core::error::LoomError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Surface name. ULID-like or human-readable; matches the file stem of
/// the `.cwasm` artifact.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
pub struct SurfaceName(pub String);

/// Failed-recovery record. Surfaced to `StartupManager` so the daemon
/// can either retry or omit the surface from `ModuleLibrary`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadFailure {
    pub surface: SurfaceName,
    pub artifact_path: PathBuf,
    pub error_code: String,
    pub details: String,
}

/// The library.
pub struct ModuleLibrary {
    pub(crate) runtime: Arc<WasmRuntime>,
    pub(crate) surfaces_dir: PathBuf,
    pub(crate) inner:
        parking_lot::RwLock<BTreeMap<SurfaceName, Arc<wasmtime::component::Component>>>,
    pub(crate) failed: parking_lot::RwLock<Vec<LoadFailure>>,
}

impl ModuleLibrary {
    /// Construct an empty library; call `load_all` to populate.
    pub fn new(runtime: Arc<WasmRuntime>, surfaces_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            surfaces_dir,
            inner: parking_lot::RwLock::new(BTreeMap::new()),
            failed: parking_lot::RwLock::new(Vec::new()),
        })
    }

    /// Load every `.cwasm` artifact in `surfaces_dir`. Returns the list
    /// of failures (per-surface independent — one bad artifact does not
    /// block the rest, per design §3.3).
    pub fn load_all(&self) -> Result<Vec<LoadFailure>, LoomError> {
        use loom_core::error::LoomErrorCode;
        let read_dir = match std::fs::read_dir(&self.surfaces_dir) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(LoomError::new(LoomErrorCode::Io, e.to_string())),
        };
        let mut failures = Vec::new();
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("cwasm") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_owned();
            let name = SurfaceName(stem);
            if let Err(e) = self.load_one(&name, &path) {
                failures.push(LoadFailure {
                    surface: name,
                    artifact_path: path,
                    error_code: e.code.as_wire().to_owned(),
                    details: e.message.clone(),
                });
            }
        }
        *self.failed.write() = failures.clone();
        Ok(failures)
    }

    /// Load one surface from a given artifact path. Used by recovery and
    /// `load_all`. Checks the SHA-256 sidecar against the compile-time
    /// `LOOM_SURFACE_WEB_SHA256` constant.
    pub fn load_one(&self, name: &SurfaceName, path: &Path) -> Result<(), LoomError> {
        self.load_one_with_expected_sha(name, path, env!("LOOM_SURFACE_WEB_SHA256"))
    }

    /// Testability shim for SHA-mismatch tests: load one surface while
    /// supplying the expected SHA-256 directly instead of reading the
    /// compile-time constant.
    ///
    /// Logic:
    /// - If `expected_sha` is empty → skip integrity check (dev builds where
    ///   the wasm artifact hasn't been compiled yet, per build.rs).
    /// - Otherwise → read `path` stem + `.sha256` sidecar. If the sidecar
    ///   exists and its content doesn't match `expected_sha` → `StoreIntegrityFailed`.
    pub fn load_one_with_expected_sha(
        &self,
        name: &SurfaceName,
        path: &Path,
        expected_sha: &str,
    ) -> Result<(), LoomError> {
        use loom_core::error::LoomErrorCode;

        if !expected_sha.is_empty() {
            // Sidecar convention: <name>.sha256 alongside the .cwasm artifact.
            // Built by compile_step; named to match the cwasm artifact.
            let sidecar = path.with_extension("sha256");
            if sidecar.exists() {
                let actual = std::fs::read_to_string(&sidecar)
                    .map_err(|e| LoomError::new(LoomErrorCode::Io, e.to_string()))?;
                if actual.trim() != expected_sha {
                    return Err(LoomError::new(
                        LoomErrorCode::StoreIntegrityFailed,
                        format!(
                            "surface '{}' SHA-256 mismatch: sidecar has '{}', expected '{}'",
                            name.0,
                            actual.trim(),
                            expected_sha,
                        ),
                    ));
                }
            }
        }

        let component = unsafe {
            wasmtime::component::Component::deserialize_file(self.runtime.engine(), path)
                .map_err(|e| LoomError::new(LoomErrorCode::StoreIntegrityFailed, e.to_string()))?
        };
        self.inner.write().insert(name.clone(), Arc::new(component));
        Ok(())
    }

    /// Read-lock lookup. Returns `LoomError::Unsupported` (surface unavailable)
    /// when the surface was not loaded — NEVER compiles on demand.
    pub fn get(
        &self,
        name: &SurfaceName,
    ) -> Result<Arc<wasmtime::component::Component>, LoomError> {
        use loom_core::error::LoomErrorCode;
        self.inner.read().get(name).cloned().ok_or_else(|| {
            LoomError::new(
                LoomErrorCode::Unsupported,
                format!(
                    "surface '{}' not loaded — use compile_module to install first",
                    name.0
                ),
            )
        })
    }

    /// Check whether a surface is loaded. Used by `WasmHost` for the
    /// pre-dispatch check.
    pub fn contains(&self, name: &SurfaceName) -> bool {
        self.inner.read().contains_key(name)
    }

    /// Count of successfully-loaded surfaces. Used by `WasmHost::new` for
    /// startup observability — distinguishes "no .cwasm files on disk"
    /// from "files were there but all failed to load".
    pub fn loaded_count(&self) -> usize {
        self.inner.read().len()
    }

    /// Snapshot of failed-load records.
    pub fn failures(&self) -> Vec<LoadFailure> {
        self.failed.read().clone()
    }

    /// Recovery write-lock entrypoint. Called by `StartupManager` AFTER
    /// `Compiler::compile` succeeded on the source bundle.
    pub fn install_recovered(
        &self,
        name: SurfaceName,
        component: Arc<wasmtime::component::Component>,
    ) -> Result<(), LoomError> {
        self.inner.write().insert(name, component);
        Ok(())
    }
}
