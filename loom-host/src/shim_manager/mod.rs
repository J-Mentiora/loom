//! `shim_manager` — re-exports the implementation submodule.
pub mod process;
pub mod shim_manager;
pub use shim_manager::*;

#[cfg(test)]
mod interface_tests;
