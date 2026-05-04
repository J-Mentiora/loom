//! `navigate_verb` — see `systems/loom-surfaces/modules/navigate_verb/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod navigate_verb;
pub use navigate_verb::*;

#[cfg(test)]
mod interface_tests;
