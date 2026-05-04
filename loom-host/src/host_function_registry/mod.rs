//! `host_function_registry` — see `systems/loom-host/modules/host_function_registry/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod host_function_registry;
pub use host_function_registry::*;

#[cfg(test)]
mod interface_tests;
