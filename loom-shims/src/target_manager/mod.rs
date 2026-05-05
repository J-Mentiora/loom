//! `target_manager` — see `systems/loom-shims/modules/target_manager/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod target_manager;
pub use target_manager::*;

#[cfg(test)]
mod interface_tests;
