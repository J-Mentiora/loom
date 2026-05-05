//! `version_command` — re-exports the implementation submodule.
pub mod version_command;
pub use version_command::*;

#[cfg(test)]
mod interface_tests;
