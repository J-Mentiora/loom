// RpcHandlers — one handler function per canonical RPC method
// (loom-rpc_contract.md). Each handler routes to either
// `CoreServiceAdapter` (session.* / vault.*) or `HostServiceAdapter`
// (action.<surface>.<verb>) and serialises the result via
// `serde_jcs::to_string` (RFC 8785 canonical JSON).
//
// # Contract semantics
// - **Single dispatch table.** Every method in
//   `loom-rpc_contract.md` maps to one `RpcHandlers::*` async
//   function. The handler set is registered onto a `jsonrpsee::RpcModule`
//   via `RequestRouter::register_methods` at startup.
// - **Routing.** `action.*` handlers call
//   `HostServiceAdapter::dispatch_action`. `session.*` / `vault.*`
//   handlers call `CoreServiceAdapter`. Misrouting will not type-check
//   because the two adapters have incompatible return types.
// - **Canonical JSON.** All response bodies serialised
//   via `serde_jcs::to_string`. Clippy lint `disallowed_methods` bans
//   `serde_json::to_string` outside test code (per the wire-spec's
//   schema-source-of-truth rule).
// - **Errors.** Adapter `LoomError` results are converted via
//   `ErrorTranslator::from_loom_error`. Schema-violation envelopes
//   for vault.grant responses are produced via
//   `SchemaValidator::validate_response` (belt+braces response check).

use crate::core_service_adapter::core_service_adapter::CoreServiceAdapterApi;
pub use crate::error_translator::error_translator::JsonRpcError;
use crate::host_service_adapter::host_service_adapter::HostServiceAdapterApi;
use crate::rpc_observability::rpc_observability::RpcObservabilityApi;
use crate::schema_provider::schema_provider::SchemaProviderApi;
use crate::schema_validator::schema_validator::SchemaValidatorApi;
use std::sync::Arc;

/// Result type returned by every handler. `Err` carries an already-built
/// JSON-RPC error envelope so the connection-handler layer can encode
/// it directly without re-translating.
pub type HandlerResult<T> = Result<T, JsonRpcError>;

/// Bundle of `Arc` handles needed by handlers. Built once at startup
/// and shared via `Arc<RpcHandlers>` across all per-connection tasks.
#[allow(dead_code)]
pub struct RpcHandlers {
    pub(crate) core: Arc<dyn CoreServiceAdapterApi>,
    pub(crate) host: Arc<dyn HostServiceAdapterApi>,
    pub(crate) schemas: Arc<dyn SchemaProviderApi>,
    pub(crate) validator: Arc<dyn SchemaValidatorApi>,
    pub(crate) observability: Arc<dyn RpcObservabilityApi>,
}
