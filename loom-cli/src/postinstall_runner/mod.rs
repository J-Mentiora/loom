//! `postinstall_runner` — re-exports the implementation submodule.
pub mod postinstall_runner;
pub use postinstall_runner::*;

#[cfg(test)]
mod interface_tests;
