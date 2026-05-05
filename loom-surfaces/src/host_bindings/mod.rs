//! `host_bindings` — see `systems/loom-surfaces/modules/host_bindings/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod host_bindings;
pub use host_bindings::*;

#[cfg(test)]
mod interface_tests;
