//! `action_executor` — re-exports the implementation submodule.
pub mod action_executor;
pub use action_executor::*;

#[cfg(test)]
mod interface_tests;
