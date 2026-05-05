//! `command_router` — see `systems/loom-cli/modules/CommandRouter/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod command_router;
pub mod validate_flags;
pub use command_router::*;
pub use validate_flags::validate_flags;

#[cfg(test)]
mod interface_tests;
