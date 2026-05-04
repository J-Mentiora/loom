//! `session_executor` — see `systems/loom-host/modules/session_executor/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod session_executor;
pub use session_executor::*;

#[cfg(test)]
mod interface_tests;
