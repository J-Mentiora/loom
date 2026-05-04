//! `snapshot_verb` — see `systems/loom-surfaces/modules/snapshot_verb/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod snapshot_verb;
pub use snapshot_verb::*;

#[cfg(test)]
mod interface_tests;
