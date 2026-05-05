//! `rpc_client` — see `systems/loom-cli/modules/RpcClient/interfaces.rs`
//! for the locked v5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod rpc_client;
pub use rpc_client::*;

#[cfg(test)]
mod interface_tests;
