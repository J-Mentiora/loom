//! `observability` — re-exports the implementation submodule.
pub mod observability;
pub use observability::*;

#[cfg(test)]
mod interface_tests;
