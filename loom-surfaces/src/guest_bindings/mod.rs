//! `guest_bindings` — see `systems/loom-surfaces/modules/guest_bindings/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod guest_bindings;
pub use guest_bindings::*;

#[cfg(test)]
mod interface_tests;
