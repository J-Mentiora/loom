//! `output_formatter` — see `systems/loom-cli/modules/OutputFormatter/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod output_formatter;
pub use output_formatter::*;

#[cfg(test)]
mod interface_tests;
