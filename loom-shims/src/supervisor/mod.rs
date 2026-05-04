//! `supervisor` — see `systems/loom-shims/modules/supervisor/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod supervisor;
pub mod run;
pub use supervisor::*;

#[cfg(test)]
mod interface_tests;
