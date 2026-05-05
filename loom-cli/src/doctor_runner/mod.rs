//! `doctor_runner` — re-exports the implementation submodule.
pub mod doctor_runner;
pub use doctor_runner::*;

#[cfg(test)]
mod interface_tests;
