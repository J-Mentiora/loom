//! `click_verb` — see `systems/loom-surfaces/modules/click_verb/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod click_verb;
pub use click_verb::*;

#[cfg(test)]
mod interface_tests;
