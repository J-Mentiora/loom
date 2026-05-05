//! `screenshot_verb` — see `systems/loom-surfaces/modules/screenshot_verb/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod screenshot_verb;
pub use screenshot_verb::*;

#[cfg(test)]
mod interface_tests;
