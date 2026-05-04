//! `serve_runner` — see `systems/loom-cli/modules/ServeRunner/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod serve_runner;
pub use serve_runner::*;

#[cfg(test)]
mod interface_tests;
