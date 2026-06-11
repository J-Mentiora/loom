// WasmRuntime — singleton `Arc<wasmtime::Engine>`.
//
// # Contract semantics
// - **One Engine per process.** All `Component`s, `Store`s, and
//   `Linker`s in `loom-host` share this engine. Construction at
//   `WasmHost::new`; engine config is fixed for process lifetime.
// - **AOT loading + component model + fuel.** The config enables
//   `wasmtime::Config::wasm_component_model(true)`,
//   `consume_fuel(true)`, and `cranelift_opt_level(SpeedAndSize)`.
// - **Epoch-based preemption.** `epoch_interruption(true)` is ALWAYS
//   on, and `new` spawns one detached ticker thread per engine that
//   bumps `Engine::increment_epoch` every `epoch_tick_ms` (exits when
//   the engine is dropped). Every store that executes guest code MUST
//   be armed via `arm_store_preemption` — with a free-running ticker,
//   an un-armed store traps as soon as guest code runs (the default
//   epoch deadline is 0). Armed stores yield to the async executor on
//   each tick (so `SessionExecutor`'s abort/budget `select!` arm can
//   fire mid-execution) and trap after `guest_cpu_deadline_ms` worth
//   of guest-CPU ticks (runaway-loop backstop).
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
    /// Per-WASM-instance memory cap (soft default 64 MiB).
    pub mem_limit_mib: u32,
    /// Fuel budget per surface invocation (None = disabled). When Some,
    /// `arm_store_preemption` calls `Store::set_fuel` per dispatch and a
    /// guest that exhausts it traps (`Trap::OutOfFuel`).
    pub fuel_per_invocation: Option<u64>,
    /// Cranelift opt level. "speed" | "speed_and_size" | "none".
    pub opt_level: String,
    /// Epoch ticker cadence in milliseconds — the preemption granularity
    /// for CPU-bound guest code (abort/budget kill latency is ~1 tick).
    /// Clamped to >= 1.
    #[serde(default = "default_epoch_tick_ms")]
    pub epoch_tick_ms: u64,
    /// Hard per-invocation guest-CPU deadline in milliseconds; a guest
    /// that spends this much CPU time executing WASM (time inside async
    /// host calls does NOT accrue) traps with `Trap::Interrupt` through
    /// the normal trap path. Backstop for runaway loops. Clamped so at
    /// least one tick is allowed.
    #[serde(default = "default_guest_cpu_deadline_ms")]
    pub guest_cpu_deadline_ms: u64,
}

fn default_epoch_tick_ms() -> u64 {
    10
}

fn default_guest_cpu_deadline_ms() -> u64 {
    30_000
}

impl Default for WasmRuntimeConfig {
    fn default() -> Self {
        Self {
            mem_limit_mib: 64,
            fuel_per_invocation: None,
            opt_level: "speed_and_size".into(),
            epoch_tick_ms: default_epoch_tick_ms(),
            guest_cpu_deadline_ms: default_guest_cpu_deadline_ms(),
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
        // Always on: without epoch interruption a CPU-bound guest loop is
        // unpreemptable — `Func::call_async` never yields, so the abort /
        // budget-kill `select!` arm in SessionExecutor can never fire.
        wt.epoch_interruption(true);
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
        // One ticker thread per engine. Holds only an `EngineWeak` so it
        // exits when the last strong engine handle is dropped. Fail loud on
        // spawn failure: an engine without a ticker would silently leave
        // armed stores unpreemptable (the epoch never advances).
        let weak = engine.weak();
        let tick = std::time::Duration::from_millis(config.epoch_tick_ms.max(1));
        std::thread::Builder::new()
            .name("loom-epoch-ticker".into())
            .spawn(move || loop {
                std::thread::sleep(tick);
                match weak.upgrade() {
                    Some(engine) => engine.increment_epoch(),
                    None => break,
                }
            })
            .map_err(|e| {
                LoomError::new(
                    LoomErrorCode::Internal,
                    format!("failed to spawn epoch ticker thread: {e}"),
                )
            })?;
        Ok(Arc::new(WasmRuntime { engine, config }))
    }

    /// Arm per-invocation preemption on a fresh `Store`. MUST be called
    /// before any guest code runs on it (instantiate included) — the
    /// engine's epoch ticker is free-running, so an un-armed store traps
    /// immediately at the first epoch check.
    ///
    /// Behaviour armed here:
    /// - yield to the async executor every epoch tick, so the
    ///   `tokio::select!` racing `Func::call_async` against the abort
    ///   signal is re-polled within ~`epoch_tick_ms` even while the
    ///   guest is CPU-bound;
    /// - after `guest_cpu_deadline_ms` worth of guest-CPU ticks, raise
    ///   `Trap::Interrupt` (runaway-loop backstop; surfaces through the
    ///   existing `TrapHandler` path as a trapped-action receipt);
    /// - when `fuel_per_invocation` is Some, load that fuel budget into
    ///   the store (`consume_fuel` was enabled at engine construction).
    pub fn arm_store_preemption<T>(&self, store: &mut wasmtime::Store<T>) -> Result<(), LoomError> {
        use loom_core::error::LoomErrorCode;
        let tick_ms = self.config.epoch_tick_ms.max(1);
        let deadline_ticks = (self.config.guest_cpu_deadline_ms / tick_ms).max(1);
        store.set_epoch_deadline(1);
        let mut elapsed_ticks: u64 = 0;
        store.epoch_deadline_callback(move |_cx| {
            elapsed_ticks += 1;
            if elapsed_ticks >= deadline_ticks {
                Ok(wasmtime::UpdateDeadline::Interrupt)
            } else {
                Ok(wasmtime::UpdateDeadline::Yield(1))
            }
        });
        if let Some(fuel) = self.config.fuel_per_invocation {
            store.set_fuel(fuel).map_err(|e| {
                LoomError::new(
                    LoomErrorCode::Internal,
                    format!("Store::set_fuel({fuel}) failed: {e}"),
                )
            })?;
        }
        Ok(())
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
        Ok(format!(
            "wh-{}-{}",
            std::env::consts::ARCH,
            self.config.opt_level
        ))
    }
}
