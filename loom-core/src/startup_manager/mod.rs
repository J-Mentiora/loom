//! `startup_manager` — see `systems/loom-core/modules/startup_manager/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod startup_manager;
pub use startup_manager::*;

mod impl_local;

#[cfg(test)]
mod interface_tests;
