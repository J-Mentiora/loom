//! `version_command` — see `systems/loom-cli/modules/VersionCommand/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod version_command;
pub use version_command::*;

#[cfg(test)]
mod interface_tests;
