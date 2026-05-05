//! `host_bindings` — re-exports the implementation submodule.
pub mod host_bindings;
pub use host_bindings::*;

#[cfg(test)]
mod interface_tests;
