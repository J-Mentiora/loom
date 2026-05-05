//! `host_function_table` — see crate root.
pub mod host_function_table;
pub use host_function_table::*;

mod host_impl;

#[cfg(test)]
mod interface_tests;
