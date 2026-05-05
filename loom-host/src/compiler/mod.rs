//! `compiler` — see `systems/loom-host/modules/compiler/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod compiler;
pub use compiler::*;

#[cfg(test)]
mod interface_tests;
