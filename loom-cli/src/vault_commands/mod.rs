//! `vault_commands` — re-exports the implementation submodule.
pub mod vault_commands;
pub use vault_commands::*;

#[cfg(test)]
mod interface_tests;
