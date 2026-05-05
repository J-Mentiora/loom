//! `select_verb` — see `systems/loom-surfaces/modules/select_verb/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod select_verb;
pub use select_verb::*;

#[cfg(test)]
mod interface_tests;
