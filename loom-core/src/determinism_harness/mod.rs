//! `determinism_harness` — re-exports the implementation submodule.
pub mod determinism_harness;
pub use determinism_harness::*;

mod impl_harness;

#[cfg(test)]
mod interface_tests;
