//! `module_library` — see `systems/loom-host/modules/module_library/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod module_library;
pub use module_library::*;

#[cfg(test)]
mod interface_tests;
