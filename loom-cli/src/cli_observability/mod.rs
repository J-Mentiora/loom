//! `cli_observability` — re-exports the implementation submodule.
pub mod cli_observability;
pub use cli_observability::*;

#[cfg(test)]
mod interface_tests;
