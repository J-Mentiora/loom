//! `postinstall_runner` — see `systems/loom-cli/modules/PostinstallRunner/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod postinstall_runner;
pub use postinstall_runner::*;

#[cfg(test)]
mod interface_tests;
