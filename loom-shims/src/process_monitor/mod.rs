//! `process_monitor` — see `systems/loom-shims/modules/process_monitor/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod process_monitor;
pub use process_monitor::*;

#[cfg(test)]
mod interface_tests;
