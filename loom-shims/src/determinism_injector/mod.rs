//! `determinism_injector` — see `systems/loom-shims/modules/determinism_injector/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod determinism_injector;
pub use determinism_injector::*;

#[cfg(test)]
mod interface_tests;
