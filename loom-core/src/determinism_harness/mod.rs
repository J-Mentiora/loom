//! `determinism_harness` — see `systems/loom-core/modules/determinism_harness/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod determinism_harness;
pub use determinism_harness::*;

mod impl_harness;

#[cfg(test)]
mod interface_tests;
