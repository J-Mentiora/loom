//! `replay_engine` — see `systems/loom-core/modules/replay_engine/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod replay_engine;
pub use replay_engine::*;

mod impl_replay;

#[cfg(test)]
mod interface_tests;
