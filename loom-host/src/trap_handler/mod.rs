//! `trap_handler` — see `systems/loom-host/modules/trap_handler/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod trap_handler;
pub use trap_handler::*;

#[cfg(test)]
mod interface_tests;
