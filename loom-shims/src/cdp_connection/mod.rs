//! `cdp_connection` — see `systems/loom-shims/modules/cdp_connection/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod cbor_json;
pub mod cdp_connection;
pub use cdp_connection::*;

#[cfg(test)]
mod interface_tests;
