//! `host_function_registry` — re-exports the implementation submodule.
pub mod host_function_registry;
pub use host_function_registry::*;

#[cfg(test)]
mod interface_tests;
