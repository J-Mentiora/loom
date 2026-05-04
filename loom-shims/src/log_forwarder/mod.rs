//! `log_forwarder` — see `systems/loom-shims/modules/log_forwarder/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod log_forwarder;
pub use log_forwarder::*;

#[cfg(test)]
mod interface_tests;
