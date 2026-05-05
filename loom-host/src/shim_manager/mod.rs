//! `shim_manager` — see `systems/loom-host/modules/shim_manager/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod process;
pub mod shim_manager;
pub use shim_manager::*;

#[cfg(test)]
mod interface_tests;
