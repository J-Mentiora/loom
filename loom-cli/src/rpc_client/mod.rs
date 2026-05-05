//! `rpc_client` — re-exports the implementation submodule.
pub mod rpc_client;
pub use rpc_client::*;

#[cfg(test)]
mod interface_tests;
