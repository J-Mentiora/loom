//! `host_function_table` — see `systems/loom-host/modules/host_function_table/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod host_function_table;
pub use host_function_table::*;

mod host_impl;

#[cfg(test)]
mod interface_tests;
