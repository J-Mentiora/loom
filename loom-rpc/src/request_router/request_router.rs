// Re-export of the locked Phase 5.3 interface. DO NOT EDIT here.
// Edit `systems/loom-rpc/modules/request_router/interfaces.rs` instead.
// RequestRouter — `jsonrpsee::RpcModule` mapping method names to
// `RpcHandlers::*` functions (SR-RPC-03 / FR-PROTO-01).
//
// # Contract semantics
// - **Built once at startup.** `register_methods` walks
//   `SchemaProvider::registered_methods` and inserts a closure per
//   method into a `jsonrpsee::RpcModule<RouterContext>`. Missing
//   handler for a registered method → `RegistrationError::HandlerMissing`;
//   daemon refuses to start (SR-RPC-03). Missing schema for a
//   registered handler → `RegistrationError::SchemaMissing`.
// - **Immutable after build.** Returned `Arc<RpcModule>` is shared
//   across all per-connection tasks — no mutation on the request path.
// - **Validation runs first.** Each registered closure invokes
//   `SchemaValidatorApi::validate_request` BEFORE dispatching to the
//   handler (IC-RPC-03). On schema violation the closure short-circuits
//   and returns the envelope built by `ErrorTranslator`.

use crate::rpc_handlers::rpc_handlers::RpcHandlers;
use crate::schema_validator::schema_validator::SchemaValidatorApi;
use jsonrpsee::RpcModule;
use std::sync::Arc;

/// Per-call context the router exposes to each registered method
/// closure. Carries the handler bundle and validation hook.
pub struct RouterContext {
    pub handlers: Arc<RpcHandlers>,
    pub validator: Arc<dyn SchemaValidatorApi>,
}

#[derive(Debug)]
pub enum RegistrationError {
    /// Schema registry includes a method with no registered handler.
    HandlerMissing { method: String },
    /// Handler exists but no schema is registered. (Daemon refuses
    /// to start in either direction.)
    SchemaMissing { method: String },
    /// `jsonrpsee` registration failed (duplicate method name).
    JsonRpsee { reason: String },
}

/// Trait surface so `ConnectionHandler` can be tested with a fake
/// router that always returns a stub response.
#[async_trait::async_trait]
pub trait RequestRouterApi: Send + Sync {
    /// Dispatch a parsed JSON-RPC request to the matching handler.
    /// `method` is the JSON-RPC method name; `params` is the
    /// already-deserialised parameter `Value`. Returns the canonical
    /// JSON response body bytes.
    async fn dispatch(&self, method: &str, params: serde_json::Value) -> Vec<u8>;

    /// Enumerate every registered method. `loom serve` uses this to
    /// emit a startup audit line.
    fn methods(&self) -> Vec<String>;
}

#[allow(dead_code)]
pub struct RequestRouter {
    pub(crate) module: Arc<RpcModule<RouterContext>>,
    pub(crate) ctx: Arc<RouterContext>,
    pub(crate) methods: Vec<String>,
}
