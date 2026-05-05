//! `ipc_endpoint` — see `systems/loom-shims/modules/ipc_endpoint/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod ipc_endpoint;
pub mod runner;
pub use ipc_endpoint::*;

#[cfg(test)]
mod interface_tests;
