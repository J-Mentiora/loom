//! `evaluate_verb` — see `systems/loom-surfaces/modules/evaluate_verb/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod evaluate_verb;
pub use evaluate_verb::*;

#[cfg(test)]
mod interface_tests;
