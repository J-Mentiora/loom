//! `host_observability` — see `systems/loom-host/modules/host_observability/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod host_observability;
pub use host_observability::*;

#[cfg(test)]
mod interface_tests;
