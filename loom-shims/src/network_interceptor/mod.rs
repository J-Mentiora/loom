//! `network_interceptor` — see `systems/loom-shims/modules/network_interceptor/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod network_interceptor;
pub use network_interceptor::*;

#[cfg(test)]
mod interface_tests;
