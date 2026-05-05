//! `cdp_connection` — see crate root.
pub mod cbor_json;
pub mod cdp_connection;
pub use cdp_connection::*;

#[cfg(test)]
mod interface_tests;
