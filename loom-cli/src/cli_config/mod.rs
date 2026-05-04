//! `cli_config` — see `systems/loom-cli/modules/ConfigResolver/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod cli_config;
pub use cli_config::*;

#[cfg(test)]
mod interface_tests;
