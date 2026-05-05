//! `ipc_endpoint` — see crate root.
pub mod ipc_endpoint;
pub mod runner;
pub use ipc_endpoint::*;

#[cfg(test)]
mod interface_tests;
