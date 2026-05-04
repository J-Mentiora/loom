// Re-export of the locked Phase 5.3 interface. DO NOT EDIT here.
// Edit `systems/loom-host/modules/wasm_runtime/interfaces.rs` instead.
// WasmRuntime — singleton `Arc<wasmtime::Engine>`.
//
// # Contract semantics
// - **One Engine per process.** All `Component`s, `Store`s, and
//   `Linker`s in `loom-host` share this engine. Construction at
//   `WasmHost::new`; engine config is fixed for process lifetime.
// - **AOT loading + component model + fuel.** The config enables
//   `wasmtime::Config::wasm_component_model(true)`,
//   `consume_fuel(true)`, and `cranelift_opt_level(SpeedAndSize)`.
// - **Cwasm precompile signature is part of the artifact path.** The
//   engine's `precompile_compatibility_hash` is folded into the cache
//   key; a wasmtime version bump invalidates artifacts and triggers
//   `StartupManager` recovery (§3.3).
// - **No surfaces of `wasmtime::Engine` directly to higher modules.**
//   `WasmHost`, `Compiler`, `ModuleLibrary`, `SessionExecutor` accept
//   `Arc<WasmRuntime>` and call `.engine()` to get the engine handle.
//   This boundary keeps wasmtime upgrades scoped to one module.

use loom_core::error::LoomError;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Configuration for the singleton engine. Read from `HostConfig` at
/// `WasmHost::new`; not mutable thereafter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmRuntimeConfig {
    /// Per-WASM-instance memory cap (BC §6 soft default 64 MiB).
    pub mem_limit_mib: u32,
    /// Fuel budget per surface invocation (None = disabled).
    pub fuel_per_invocation: Option<u64>,
    /// Cranelift opt level. "speed" | "speed_and_size" | "none".
    pub opt_level: String,
}

impl Default for WasmRuntimeConfig {
    fn default() -> Self {
        Self {
            mem_limit_mib: 64,
            fuel_per_invocation: None,
            opt_level: "speed_and_size".into(),
        }
    }
}

/// The runtime singleton.
pub struct WasmRuntime {
    pub(crate) engine: wasmtime::Engine,
    pub(crate) config: WasmRuntimeConfig,
}

impl WasmRuntime {
    /// Construct the singleton. Failure is `LoomError::WasmRuntimeInit`.
    pub fn new(config: WasmRuntimeConfig) -> Result<Arc<Self>, LoomError> {
        use loom_core::error::LoomErrorCode;
        let mut wt = wasmtime::Config::new();
        wt.wasm_component_model(true);
        // async_support removed in wasmtime 44 (no-op; async is a compile-time feature)
        wt.consume_fuel(config.fuel_per_invocation.is_some());
        let opt = match config.opt_level.as_str() {
            "none" => wasmtime::OptLevel::None,
            "speed" => wasmtime::OptLevel::Speed,
            _ => wasmtime::OptLevel::SpeedAndSize,
        };
        wt.cranelift_opt_level(opt);
        let mem_bytes = (config.mem_limit_mib as u64) * 1024 * 1024;
        // static_memory_maximum_size renamed to memory_reservation in wasmtime 44
        wt.memory_reservation(mem_bytes);
        let engine = wasmtime::Engine::new(&wt)
            .map_err(|e| LoomError::new(LoomErrorCode::Internal, e.to_string()))?;
        Ok(Arc::new(WasmRuntime { engine, config }))
    }

    /// Reference to the underlying engine. Higher modules call this to
    /// build `Linker`s and `Store`s.
    pub fn engine(&self) -> &wasmtime::Engine {
        &self.engine
    }

    /// Snapshot config (used by `Compiler` to compute the cwasm path).
    pub fn config(&self) -> &WasmRuntimeConfig {
        &self.config
    }

    /// Precompile-compatibility hash. Folded into `.cwasm` artifact paths
    /// so a wasmtime version bump triggers `StartupManager` recovery.
    pub fn precompile_compatibility_hash(&self) -> Result<String, LoomError> {
        Ok(format!("wh-{}-{}", std::env::consts::ARCH, self.config.opt_level))
    }
}
