//! `help_generator` — see `systems/loom-cli/modules/HelpGenerator/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod help_generator;
pub use help_generator::*;

#[cfg(test)]
mod interface_tests;
