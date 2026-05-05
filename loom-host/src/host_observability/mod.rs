//! `host_observability` — re-exports the implementation submodule.
pub mod host_observability;
pub use host_observability::*;

#[cfg(test)]
mod interface_tests;
