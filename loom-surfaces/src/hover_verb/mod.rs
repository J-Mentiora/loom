//! `hover_verb` — see `systems/loom-surfaces/modules/hover_verb/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod hover_verb;
pub use hover_verb::*;

#[cfg(test)]
mod interface_tests;
