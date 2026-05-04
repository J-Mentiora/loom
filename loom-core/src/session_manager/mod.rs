//! `session_manager` — see `systems/loom-core/modules/session_manager/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod session_manager;
pub use session_manager::*;

mod impl_local;

#[cfg(test)]
mod interface_tests;
