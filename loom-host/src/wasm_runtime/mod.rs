//! `wasm_runtime` — see `systems/loom-host/modules/wasm_runtime/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod wasm_runtime;
pub use wasm_runtime::*;

#[cfg(test)]
mod interface_tests;
