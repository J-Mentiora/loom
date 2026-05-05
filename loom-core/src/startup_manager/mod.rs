//! `startup_manager` — see crate root.
pub mod startup_manager;
pub use startup_manager::*;

mod impl_local;

#[cfg(test)]
mod interface_tests;
