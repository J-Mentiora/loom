//! `admin_commands` — see `systems/loom-cli/modules/AdminCommands/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod admin_commands;
pub use admin_commands::*;

#[cfg(test)]
mod interface_tests;
