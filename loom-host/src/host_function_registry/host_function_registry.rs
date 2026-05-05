// Re-export of the locked v5.3 interface. DO NOT EDIT here.
// Edit `systems/loom-host/modules/host_function_registry/interfaces.rs` instead.
// HostFunctionRegistry — builds two pre-baked `wasmtime::component::Linker<HostState>`
// instances at startup; selects per-session linker by `Mode`.
//
// # Contract semantics
// - **Two pre-built linkers.** `live_linker` and
//   `replay_linker` are constructed ONCE at `WasmHost::new`, both
//   immutable for the process lifetime. Mode selection is a pointer
//   choice, not a table mutation.
// - **Generated `add_to_linker`.** The `wit-bindgen`
//   output supplies one `add_to_linker<T, U: Host>(linker, get)`
//   function. We invoke it twice, once with `LiveHostFns` and once
//   with `ReplayHostFns` (both impl the generated `host::Host` trait
//   on `HostState`).
// - **Acyclic dep block.** This module depends ONLY on
//   `HostFunctionTable` (per `module_list.md`). It does NOT depend on
//   `WasmRuntime` directly — the engine is passed in via the linker's
//   constructor argument.
// - **Replay never reaches live side-effects.** The
//   `replay_linker` registers `ReplayHostFns`, which has no edges into
//   `Vault`, `ShimManager::send`, live HTTP, or `ContentStore::put`.

use crate::host_function_table::HostState;
use crate::wit_type_marshaller::Mode;
use loom_core::error::LoomError;
use std::sync::Arc;

pub struct HostFunctionRegistry {
    pub(crate) live_linker: wasmtime::component::Linker<HostState>,
    pub(crate) replay_linker: wasmtime::component::Linker<HostState>,
}

impl HostFunctionRegistry {
    /// Build both linkers. Called once at `WasmHost::new`.
    /// Both linkers register the same `HostState` host-fn implementation;
    /// mode dispatch is inside `HostState`'s generated `Host` trait impl.
    pub fn new(engine: &wasmtime::Engine) -> Result<Arc<Self>, LoomError> {
        use crate::wit_type_marshaller::loom_surface_bindings;
        use loom_core::error::LoomErrorCode;

        let mut live_linker = wasmtime::component::Linker::<HostState>::new(engine);
        loom_surface_bindings::loom::surface::host::add_to_linker::<
            _,
            wasmtime::component::HasSelf<HostState>,
        >(&mut live_linker, |s: &mut HostState| s)
        .map_err(|e| LoomError::new(LoomErrorCode::Internal, e.to_string()))?;
        // Surfaces compiled for `wasm32-wasip2` transitively import
        // `wasi:io/poll@0.2.6` (and friends) via Rust stdlib alloc/format.
        // Register the wasi:p2 interfaces so instantiation succeeds; the
        // sandbox itself is enforced by the per-store `WasiCtx`
        // constructed via `build_sandboxed_wasi_ctx`.
        wasmtime_wasi::p2::add_to_linker_async(&mut live_linker)
            .map_err(|e| LoomError::new(LoomErrorCode::Internal, e.to_string()))?;

        let mut replay_linker = wasmtime::component::Linker::<HostState>::new(engine);
        loom_surface_bindings::loom::surface::host::add_to_linker::<
            _,
            wasmtime::component::HasSelf<HostState>,
        >(&mut replay_linker, |s: &mut HostState| s)
        .map_err(|e| LoomError::new(LoomErrorCode::Internal, e.to_string()))?;
        wasmtime_wasi::p2::add_to_linker_async(&mut replay_linker)
            .map_err(|e| LoomError::new(LoomErrorCode::Internal, e.to_string()))?;

        Ok(Arc::new(Self {
            live_linker,
            replay_linker,
        }))
    }

    /// Pick a linker by mode. Pointer comparison only — no table mutation.
    pub fn linker_for(&self, mode: Mode) -> &wasmtime::component::Linker<HostState> {
        match mode {
            Mode::Live => &self.live_linker,
            Mode::Replay => &self.replay_linker,
        }
    }

    /// Test seam — used by `interface_tests.rs` to pin the two-linker
    /// invariant without instantiating wasmtime.
    pub fn linker_count() -> usize {
        2
    }
}
