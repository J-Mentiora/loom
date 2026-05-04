//! `cli_main` — see `systems/loom-cli/modules/main/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod cli_main;
pub use cli_main::*;

#[cfg(test)]
mod interface_tests;
