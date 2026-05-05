pub mod rpc_client;
pub use rpc_client::*;

#[cfg(test)]
mod interface_tests;

use crate::error_mapper::{ErrorMapper, McpContent, ToolResult};
use loom_rpc::error::{LoomError, LoomErrorCode};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Concrete framed-socket caller
// ---------------------------------------------------------------------------

struct FramedCaller {
    stream: tokio::sync::Mutex<loom_rpc::frame_handler::FramedUnixStream>,
    next_id: AtomicU64,
}

#[async_trait::async_trait]
impl JsonRpcCaller for FramedCaller {
    async fn raw_call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, LoomError> {
        use futures::{SinkExt, StreamExt};
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": id,
        });
        let req_bytes =
            serde_json::to_vec(&req).map_err(|e| ErrorMapper::from_rpc_io(&e.to_string()))?;
        let mut stream = self.stream.lock().await;
        stream
            .send(bytes::Bytes::from(req_bytes))
            .await
            .map_err(|e| ErrorMapper::from_rpc_io(&e.to_string()))?;
        let frame = stream
            .next()
            .await
            .ok_or_else(|| ErrorMapper::from_rpc_io("connection closed"))?
            .map_err(|e| ErrorMapper::from_rpc_io(&e.to_string()))?;
        let resp: serde_json::Value =
            serde_json::from_slice(&frame).map_err(|e| ErrorMapper::from_rpc_io(&e.to_string()))?;
        if let Some(err_val) = resp.get("error") {
            let loom_err: LoomError = serde_json::from_value(err_val.clone())
                .unwrap_or_else(|_| ErrorMapper::from_rpc_io("malformed error response"));
            return Err(loom_err);
        }
        Ok(resp
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null))
    }
}

// ---------------------------------------------------------------------------
// RpcClientConfig
// ---------------------------------------------------------------------------

impl RpcClientConfig {
    pub fn defaults() -> Self {
        let socket_path = dirs::cache_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
            .join("loom/loom.sock");
        let hello_token_path = dirs::data_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
            .join("loom/auth/hello.token");
        Self {
            socket_path,
            hello_token_path,
            backoff_initial: Duration::from_millis(100),
            backoff_cap: Duration::from_secs(5),
        }
    }
}

// ---------------------------------------------------------------------------
// RpcClient
// ---------------------------------------------------------------------------

impl RpcClient {
    pub fn new(
        cfg: RpcClientConfig,
        obs: Arc<crate::mcp_observability::McpObservability>,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Arc::new(tokio::sync::RwLock::new(ConnectionState::Connecting)),
            inner: Arc::new(tokio::sync::RwLock::new(None)),
            cfg,
            obs,
            on_connected: Arc::new(tokio::sync::RwLock::new(vec![])),
            reconnect_task: Arc::new(tokio::sync::Mutex::new(None)),
        })
    }

    pub async fn connect(self: &Arc<Self>) -> Result<(), LoomError> {
        {
            let state = self.state.read().await;
            if *state == ConnectionState::Connected {
                return Ok(());
            }
        }
        {
            let mut state = self.state.write().await;
            *state = ConnectionState::Connecting;
        }

        let token = Self::read_hello_token(&self.cfg.hello_token_path)?;
        let stream = tokio::net::UnixStream::connect(&self.cfg.socket_path)
            .await
            .map_err(|e| ErrorMapper::from_rpc_io(&format!("connect: {e}")))?;
        let mut framed = loom_rpc::frame_handler::FrameHandler::wrap_stream(stream);

        let hello = format!("HELLO {token}");
        {
            use futures::SinkExt;
            framed
                .send(bytes::Bytes::from(hello.into_bytes()))
                .await
                .map_err(|e| ErrorMapper::from_rpc_io(&format!("hello send: {e}")))?;
        }

        // Wait up to 5 s for an error response; timeout → Connected.
        let timeout_result = tokio::time::timeout(Duration::from_secs(5), async {
            use futures::StreamExt;
            framed.next().await
        })
        .await;

        match timeout_result {
            Err(_timeout) => {} // No response = HELLO accepted.
            Ok(None) => {
                return Err(ErrorMapper::from_hello_mismatch(
                    "server closed connection after HELLO",
                ));
            }
            Ok(Some(Err(e))) => {
                return Err(ErrorMapper::from_rpc_io(&format!("hello recv: {e}")));
            }
            Ok(Some(Ok(_frame))) => {
                return Err(ErrorMapper::from_hello_mismatch("server rejected HELLO"));
            }
        }

        let caller = FramedCaller {
            stream: tokio::sync::Mutex::new(framed),
            next_id: AtomicU64::new(1),
        };
        {
            let mut inner = self.inner.write().await;
            *inner = Some(RpcClientInner {
                handle: Box::new(caller),
            });
        }
        {
            let mut state = self.state.write().await;
            *state = ConnectionState::Connected;
        }

        let callbacks = self.on_connected.read().await.clone();
        for cb in &callbacks {
            cb();
        }
        Ok(())
    }

    pub async fn state(&self) -> ConnectionState {
        *self.state.read().await
    }

    pub async fn call(
        self: &Arc<Self>,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, LoomError> {
        {
            let state = self.state.read().await;
            if *state != ConnectionState::Connected {
                return Err(LoomError::new(LoomErrorCode::Io, "daemon not connected"));
            }
        }
        let result = {
            let inner = self.inner.read().await;
            match inner.as_ref() {
                None => Err(LoomError::new(LoomErrorCode::Io, "no active connection")),
                Some(i) => i.handle.raw_call(method, params).await,
            }
        };
        if result.is_err() {
            {
                let mut state = self.state.write().await;
                *state = ConnectionState::Disconnected;
            }
            self.spawn_reconnect();
        }
        result
    }

    pub async fn call_as_tool_result(
        self: &Arc<Self>,
        method: &str,
        params: serde_json::Value,
    ) -> ToolResult {
        match self.call(method, params).await {
            Ok(v) => ToolResult {
                is_error: false,
                content: vec![McpContent::from_json(v)],
            },
            Err(e) => ErrorMapper::to_tool_result(e),
        }
    }

    pub async fn register_on_connected(&self, cb: OnConnected) {
        self.on_connected.write().await.push(cb);
    }

    pub fn read_hello_token(path: &std::path::Path) -> Result<String, LoomError> {
        std::fs::read_to_string(path)
            .map(|s| s.trim().to_string())
            .map_err(|e| ErrorMapper::from_rpc_io(&format!("read hello token: {e}")))
    }

    pub fn next_backoff(cfg: &RpcClientConfig, failures: u32) -> Duration {
        let initial_ms = cfg.backoff_initial.as_millis() as u64;
        let shift = failures.min(20);
        let multiplier = 1u64 << shift;
        let delay_ms = initial_ms.saturating_mul(multiplier);
        let cap_ms = cfg.backoff_cap.as_millis() as u64;
        Duration::from_millis(delay_ms.min(cap_ms))
    }

    fn spawn_reconnect(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move {
            let mut failures = 0u32;
            loop {
                {
                    let s = this.state.read().await;
                    if *s == ConnectionState::Connected {
                        return;
                    }
                }
                let delay = Self::next_backoff(&this.cfg, failures);
                tokio::time::sleep(delay).await;
                match this.connect().await {
                    Ok(_) => return,
                    Err(_) => failures += 1,
                }
            }
        });
    }
}
