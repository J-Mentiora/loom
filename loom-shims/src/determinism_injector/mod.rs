//! `determinism_injector` — re-exports the implementation submodule.
pub mod determinism_injector;
pub use determinism_injector::*;

#[cfg(test)]
mod interface_tests;
