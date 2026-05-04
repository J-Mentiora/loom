//! `session_commands` — see `systems/loom-cli/modules/SessionCommands/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod session_commands;
pub use session_commands::*;

#[cfg(test)]
mod interface_tests;
