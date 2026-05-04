//! `shim_manager` — see `systems/loom-host/modules/shim_manager/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod shim_manager;
pub mod process;
pub use shim_manager::*;

#[cfg(test)]
mod interface_tests;
