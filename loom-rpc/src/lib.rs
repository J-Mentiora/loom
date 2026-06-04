//! `loom-rpc` — Unix-domain JSON-RPC server fronting `loom-core` +
//! `loom-host`. Owns auth middleware, request routing, schema
//! validation, and the connection lifecycle.
//!
//! API-exposed module: `socket_server`. Other modules are crate-public
//! so internal `crate::<module>::interfaces::Type` paths resolve.

// `loom_rpc::error` is referenced by loom-mcp; we re-export the
// canonical types from loom-shared to satisfy that path.
pub mod error {
    pub use loom_shared::error_format::{LoomError, LoomErrorCode, RetryDisposition};
}

// ---- Module declarations (14) ----
pub mod action_registry;
pub mod auth_middleware;
pub mod connection_handler;
pub mod core_service_adapter;
pub mod error_translator;
pub mod frame_handler;
pub mod host_service_adapter;
pub mod mcp_adapter;
pub mod request_router;
pub mod rpc_handlers;
pub mod rpc_observability;
pub mod schema_provider;
pub mod schema_validator;
pub mod session_validation;
pub mod socket_server;

#[cfg(any(test, feature = "mock"))]
pub mod mocks;

pub use socket_server::*;
