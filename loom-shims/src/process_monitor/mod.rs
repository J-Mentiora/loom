//! `process_monitor` — re-exports the implementation submodule.
pub mod process_monitor;
pub use process_monitor::*;

#[cfg(test)]
mod interface_tests;
