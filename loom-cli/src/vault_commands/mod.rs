//! `vault_commands` — see `systems/loom-cli/modules/VaultCommands/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod vault_commands;
pub use vault_commands::*;

#[cfg(test)]
mod interface_tests;
