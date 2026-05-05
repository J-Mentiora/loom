//! `doctor_runner` — see `systems/loom-cli/modules/DoctorRunner/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod doctor_runner;
pub use doctor_runner::*;

#[cfg(test)]
mod interface_tests;
