//! `scroll_verb` — see `systems/loom-surfaces/modules/scroll_verb/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod scroll_verb;
pub use scroll_verb::*;

#[cfg(test)]
mod interface_tests;
