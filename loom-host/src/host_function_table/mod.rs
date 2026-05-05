//! `host_function_table` — re-exports the implementation submodule.
pub mod host_function_table;
pub use host_function_table::*;

mod host_impl;

#[cfg(test)]
mod interface_tests;
