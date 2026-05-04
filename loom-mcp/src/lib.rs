//! `loom-mcp` — Model Context Protocol bridge over stdio. Translates
//! MCP tool calls into loom-rpc JSON-RPC requests over the daemon's
//! Unix socket.

pub mod error {
    pub use loom_shared::error_format::{LoomError, LoomErrorCode};
}

// ---- Module declarations (8) ----
pub mod error_mapper;
pub mod mcp_dispatcher;
pub mod mcp_main;
pub mod mcp_observability;
pub mod resource_tracker;
pub mod rpc_client;
pub mod stdio_transport;
pub mod tool_cache;

#[cfg(any(test, feature = "mock"))]
pub mod mocks;

pub use mcp_dispatcher::*;
