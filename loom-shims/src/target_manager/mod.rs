//! `target_manager` — re-exports the implementation submodule.
pub mod target_manager;
pub use target_manager::*;

#[cfg(test)]
mod interface_tests;
