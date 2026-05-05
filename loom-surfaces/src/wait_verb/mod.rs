//! `wait_verb` — see `systems/loom-surfaces/modules/wait_verb/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod wait_verb;
pub use wait_verb::*;

#[cfg(test)]
mod interface_tests;
