//! `mcp_delegate` — see `systems/loom-cli/modules/McpDelegate/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod mcp_delegate;
pub use mcp_delegate::*;

#[cfg(test)]
mod interface_tests;
