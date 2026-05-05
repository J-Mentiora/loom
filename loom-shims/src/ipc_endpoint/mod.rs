//! `ipc_endpoint` — re-exports the implementation submodule.
pub mod ipc_endpoint;
pub mod runner;
pub use ipc_endpoint::*;

#[cfg(test)]
mod interface_tests;
