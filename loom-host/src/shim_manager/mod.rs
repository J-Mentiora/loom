//! `shim_manager` — see crate root.
mod helpers;
pub mod process;
mod senders;
pub mod shim_manager;
mod types;
pub use shim_manager::*;

#[cfg(test)]
mod interface_tests;
