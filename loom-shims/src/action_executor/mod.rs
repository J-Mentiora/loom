//! `action_executor` — see `systems/loom-shims/modules/action_executor/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod action_executor;
pub use action_executor::*;

#[cfg(test)]
mod interface_tests;
