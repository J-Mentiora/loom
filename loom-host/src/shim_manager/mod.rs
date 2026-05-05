//! `shim_manager` — see crate root.
pub mod process;
pub mod shim_manager;
pub use shim_manager::*;

#[cfg(test)]
mod interface_tests;
