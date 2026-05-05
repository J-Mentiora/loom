//! `auth_manager` — re-exports the implementation submodule.
pub mod auth_manager;
pub use auth_manager::*;

#[cfg(test)]
mod interface_tests;
