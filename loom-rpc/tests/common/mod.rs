//! Shared test helpers for loom-rpc integration tests.
#![allow(dead_code)]

use loom_rpc::{
    auth_middleware::auth_middleware::{AuthMiddleware, Token},
    connection_handler::connection_handler::ConnectionHandlerDeps,
    request_router::request_router::RequestRouterApi,
    rpc_observability::rpc_observability::RpcObservability,
    schema_provider::schema_provider::{CompiledJsonSchema, SchemaProviderApi, SchemaRegistry},
    schema_validator::schema_validator::SchemaValidator,
};
use serde_json::json;
use std::sync::Arc;

/// Minimal no-op router for tests that don't need real dispatch.
pub struct NoopRouter;

#[async_trait::async_trait]
impl RequestRouterApi for NoopRouter {
    async fn dispatch(&self, _method: &str, _params: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&json!({"result": null})).unwrap()
    }
    fn methods(&self) -> Vec<String> {
        vec![]
    }
}

/// Minimal SchemaProvider that registers no methods.
pub struct EmptySchemaProvider;

impl SchemaProviderApi for EmptySchemaProvider {
    fn lookup_request_schema(&self, _method: &str) -> Option<Arc<CompiledJsonSchema>> {
        None
    }
    fn lookup_response_schema(&self, _method: &str) -> Option<Arc<CompiledJsonSchema>> {
        None
    }
    fn registered_methods(&self) -> Vec<String> {
        vec![]
    }
    fn get_registry_snapshot(&self) -> SchemaRegistry {
        SchemaRegistry {
            methods: vec![],
            source_wit_sha256: String::new(),
        }
    }
}

/// Build a `ConnectionHandlerDeps` suitable for tests.
/// NOTE: Token::generate() must be implemented before this can be called.
pub fn test_handler_deps() -> ConnectionHandlerDeps {
    let token = Arc::new(Token::generate());
    let auth = AuthMiddleware::new(Arc::clone(&token));
    let provider: Arc<dyn SchemaProviderApi> = Arc::new(EmptySchemaProvider);
    let validator = SchemaValidator::new(provider);
    let router: Arc<dyn RequestRouterApi> = Arc::new(NoopRouter);
    let observability = RpcObservability::new();
    ConnectionHandlerDeps {
        auth,
        validator,
        router,
        observability,
    }
}
