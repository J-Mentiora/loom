//! `cli_main` — re-exports the implementation submodule.
pub mod cli_main;
pub use cli_main::*;

#[cfg(test)]
mod interface_tests;
