//! `action_commands` — see `systems/loom-cli/modules/ActionCommands/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod action_commands;
pub use action_commands::*;

#[cfg(test)]
mod interface_tests;
