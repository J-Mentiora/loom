//! Mock harness for `loom-rpc`. Deterministic in-process JSON-RPC
//! request handling without binding a Unix socket.

use loom_shared::error_format::{LoomError, LoomErrorCode};
use serde_json::{json, Value};

pub struct MockRpcClient;

impl MockRpcClient {
    /// Stable canned response for `session.create`.
    pub fn session_create() -> Value {
        json!({
            "session_id": "01HZTESTABC123",
            "status": "created",
        })
    }

    /// Stable canned response for `session.close`.
    pub fn session_close() -> Value {
        json!({ "status": "closed" })
    }

    pub fn auth_failed() -> LoomError {
        LoomError::new(LoomErrorCode::RpcAuthFailed, "mock: bad HELLO token")
    }

    pub fn schema_violation(method: &str) -> LoomError {
        LoomError::new(
            LoomErrorCode::RpcSchemaViolation,
            format!("mock: schema rejected '{method}'"),
        )
    }
}

pub struct MockSocketServer;

impl MockSocketServer {
    pub fn bound() -> bool {
        true
    }
}
