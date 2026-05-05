//! `cdp_connection` — re-exports the implementation submodule.
pub mod cbor_json;
pub mod cdp_connection;
pub use cdp_connection::*;

#[cfg(test)]
mod interface_tests;
