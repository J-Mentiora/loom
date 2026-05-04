// RpcClient — UnixStream + LengthDelimitedCodec client to loom-rpc.
// Types only. Implementation in mod.rs.

use crate::mcp_observability::McpObservability;
use loom_rpc::error::LoomError;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Three-state FSM for the underlying socket connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    Connecting,
    Connected,
    Disconnected,
}

/// Configuration for `RpcClient`.
#[derive(Debug, Clone)]
pub struct RpcClientConfig {
    pub socket_path: PathBuf,
    pub hello_token_path: PathBuf,
    pub backoff_initial: Duration,
    pub backoff_cap: Duration,
}

/// Callback invoked after every successful handshake.
pub type OnConnected = Arc<dyn Fn() + Send + Sync>;

/// Concrete RpcClient.
pub struct RpcClient {
    pub(crate) state: Arc<tokio::sync::RwLock<ConnectionState>>,
    pub(crate) inner: Arc<tokio::sync::RwLock<Option<RpcClientInner>>>,
    pub(crate) cfg: RpcClientConfig,
    #[allow(dead_code)]
    pub(crate) obs: Arc<McpObservability>,
    pub(crate) on_connected: Arc<tokio::sync::RwLock<Vec<OnConnected>>>,
    #[allow(dead_code)]
    pub(crate) reconnect_task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

/// Opaque inner holding the live connection handle.
pub struct RpcClientInner {
    pub(crate) handle: Box<dyn JsonRpcCaller + Send + Sync>,
}

/// Substitution boundary for the underlying JSON-RPC transport.
#[async_trait::async_trait]
pub trait JsonRpcCaller {
    async fn raw_call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, LoomError>;
}
