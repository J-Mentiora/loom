//! `auth_manager` — see `systems/loom-cli/modules/AuthManager/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod auth_manager;
pub use auth_manager::*;

#[cfg(test)]
mod interface_tests;
