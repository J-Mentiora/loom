//! `startup_manager` — re-exports the implementation submodule.
pub mod startup_manager;
pub use startup_manager::*;

mod impl_local;

#[cfg(test)]
mod interface_tests;
