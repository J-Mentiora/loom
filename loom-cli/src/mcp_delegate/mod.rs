//! `mcp_delegate` — re-exports the implementation submodule.
pub mod mcp_delegate;
pub use mcp_delegate::*;

#[cfg(test)]
mod interface_tests;
