//! `admin_commands` — re-exports the implementation submodule.
pub mod admin_commands;
pub use admin_commands::*;

#[cfg(test)]
mod interface_tests;
