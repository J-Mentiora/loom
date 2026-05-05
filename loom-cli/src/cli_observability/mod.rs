//! `cli_observability` — see `systems/loom-cli/modules/CliObservability/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod cli_observability;
pub use cli_observability::*;

#[cfg(test)]
mod interface_tests;
