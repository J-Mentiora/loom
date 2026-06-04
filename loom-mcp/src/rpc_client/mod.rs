pub mod rpc_client;
pub use rpc_client::*;

#[cfg(test)]
mod interface_tests;

use crate::error_mapper::{DispatchPhase, ErrorMapper, McpContent, ToolResult};
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
        // A send failure means the request never reached the daemon → PRE
        // dispatch → safe to retry on a fresh connection for any verb.
        stream
            .send(bytes::Bytes::from(req_bytes))
            .await
            .map_err(|_| {
                ErrorMapper::from_transport_dropped("connection lost while sending", DispatchPhase::Pre)
            })?;
        // The send completed; a read failure here means the request may have
        // been processed → POST dispatch → only idempotent verbs auto-retry.
        let frame = stream
            .next()
            .await
            .ok_or_else(|| {
                ErrorMapper::from_transport_dropped("connection closed", DispatchPhase::Post)
            })?
            .map_err(|_| {
                ErrorMapper::from_transport_dropped("connection reset", DispatchPhase::Post)
            })?;
        // A frame arrived but isn't valid JSON: the daemon responded, so the
        // request was dispatched — non-retryable protocol error, not transport.
        let resp: serde_json::Value =
            serde_json::from_slice(&frame).map_err(|_| ErrorMapper::from_malformed_response())?;
        if let Some(err_val) = resp.get("error") {
            // Tolerant decode (loom_shared::from_wire) accepts the daemon's
            // snake_case codes and maps unknowns to `internal`, so this only
            // falls back on a genuinely malformed envelope.
            let loom_err: LoomError = serde_json::from_value(err_val.clone())
                .unwrap_or_else(|_| ErrorMapper::from_malformed_response());
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
        // Defer to loom_rpc's resolver — the daemon is the binding side
        // and authoritative on path conventions (macOS:
        // ~/Library/Caches/loom/loom.sock, Linux: $XDG_RUNTIME_DIR/loom.sock
        // or /tmp/loom.sock). The previous implementation hard-coded the
        // cache_dir form on every platform, which silently broke MCP on
        // Linux installs where the daemon binds to $XDG_RUNTIME_DIR or
        // /tmp and `loom-mcp serve` resolved to ~/.cache/loom/loom.sock —
        // so every tools/call returned `{"code":"io","message":"daemon
        // not connected"}`.
        let socket_path = loom_rpc::socket_server::default_socket_path();
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
        // Daemon down / socket gone is a transient transport fault (it may be
        // restarting) → TransportDropped so callers reconnect/retry rather than
        // treating it as a hard error. Generic message (no socket path leak).
        let stream = tokio::net::UnixStream::connect(&self.cfg.socket_path)
            .await
            .map_err(|_| {
                ErrorMapper::from_transport_dropped("daemon socket unavailable", DispatchPhase::Pre)
            })?;
        let mut framed = loom_rpc::frame_handler::FrameHandler::wrap_stream(stream);

        let hello = format!("HELLO {token}");
        {
            use futures::SinkExt;
            framed
                .send(bytes::Bytes::from(hello.into_bytes()))
                .await
                .map_err(|_| {
                    ErrorMapper::from_transport_dropped(
                        "connection lost during handshake",
                        DispatchPhase::Pre,
                    )
                })?;
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
            Ok(Some(Err(_e))) => {
                return Err(ErrorMapper::from_transport_dropped(
                    "connection lost during handshake",
                    DispatchPhase::Pre,
                ));
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

    /// Issue an RPC, transparently recovering from a dropped persistent
    /// connection. A long-lived MCP client must never fail an action that a
    /// fresh connection would satisfy (the reported broken-pipe bug).
    ///
    /// Recovery is bounded and **safe-by-construction**:
    /// 1. **Reconnect-first.** If the connection isn't `Connected` (e.g. the
    ///    keepalive already noticed an idle-drop), reconnect BEFORE sending, so
    ///    the send is a genuine first attempt — never a re-send — and is safe
    ///    for every verb.
    /// 2. **Retry-once on transport drop.** If an attempt fails with
    ///    `TransportDropped`, reconnect and retry exactly once — but only when
    ///    the request provably did not execute (`dispatch_phase == "pre"`) OR
    ///    the verb is idempotent. A possibly-dispatched non-idempotent verb
    ///    (e.g. `web.click`, `session.create`) is NOT auto-resent; the typed
    ///    `transport_dropped` is surfaced so the caller decides.
    ///
    /// Non-transport errors (real page/protocol errors) are returned as-is and
    /// never trigger a reconnect.
    pub async fn call(
        self: &Arc<Self>,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, LoomError> {
        // (1) Reconnect-first: a known-dead/never-connected client establishes a
        // fresh connection so the attempt below is a true first send.
        if self.state().await != ConnectionState::Connected {
            self.connect().await?;
        }

        let err = match self.attempt_once(method, params.clone()).await {
            Ok(v) => return Ok(v),
            Err(e) => e,
        };

        // Only a transport drop is recoverable here. Everything else (a real
        // page error, a typed daemon error, a malformed envelope) is returned
        // untouched and does NOT disconnect the client.
        if err.code != LoomErrorCode::TransportDropped {
            return Err(err);
        }
        self.set_state(ConnectionState::Disconnected).await;

        // (2) Decide whether retry is safe.
        let pre_dispatch = err
            .context
            .as_ref()
            .and_then(|c| c.get("dispatch_phase"))
            .and_then(|v| v.as_str())
            == Some("pre");
        if !pre_dispatch && !verb_is_idempotent(method) {
            // Possibly-dispatched non-idempotent verb: do not auto-resend.
            self.obs.info(
                "transport_dropped: not auto-retrying non-idempotent verb",
                serde_json::json!({ "method": method }),
            );
            self.spawn_reconnect();
            return Err(err);
        }

        // Retry once on a fresh connection.
        self.obs
            .info("retry_attempt", serde_json::json!({ "method": method }));
        if self.connect().await.is_err() {
            self.spawn_reconnect();
            return Err(err);
        }
        match self.attempt_once(method, params).await {
            Ok(v) => {
                self.obs
                    .info("retry_ok", serde_json::json!({ "method": method }));
                Ok(v)
            }
            Err(e2) => {
                self.set_state(ConnectionState::Disconnected).await;
                self.spawn_reconnect();
                Err(e2)
            }
        }
    }

    /// One send/recv attempt over the current connection. Scopes the `inner`
    /// read guard so a follow-up `connect()` (which needs the write guard) can
    /// not deadlock against it.
    async fn attempt_once(
        self: &Arc<Self>,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, LoomError> {
        let inner = self.inner.read().await;
        match inner.as_ref() {
            None => Err(ErrorMapper::from_transport_dropped(
                "no active connection",
                DispatchPhase::Pre,
            )),
            Some(i) => i.handle.raw_call(method, params).await,
        }
    }

    async fn set_state(&self, s: ConnectionState) {
        *self.state.write().await = s;
    }

    /// Cheap liveness round-trip (`health.ping`) used by the keepalive task to
    /// keep a long-lived connection warm and to detect a drop proactively.
    pub async fn ping(self: &Arc<Self>) -> Result<(), LoomError> {
        self.call("health.ping", serde_json::json!({})).await.map(|_| ())
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

/// Whether an RPC method is safe to auto-resend after a *possibly-dispatched*
/// transport drop (the request may already have executed once). Read-only and
/// last-write-wins verbs are idempotent; verbs with cumulative side effects
/// (clicks, typing, uploads, session creation) are not. Unknown methods default
/// to NON-idempotent — fail safe. (A provably-not-dispatched drop retries any
/// verb regardless; see `RpcClient::call`.)
fn verb_is_idempotent(method: &str) -> bool {
    // Read-only daemon/session introspection.
    if matches!(
        method,
        "health.ping"
            | "daemon.health"
            | "session.list"
            | "session.info"
            | "session.close"
    ) {
        return true;
    }
    // Read-only / last-write-wins web verbs.
    if matches!(
        method,
        "web.navigate"
            | "web.screenshot"
            | "web.clear_cookies"
            | "web.delete_cookies"
            | "web.get_cookies"
            | "web.dom_snapshot"
            | "web.current_url"
    ) {
        return true;
    }
    // Generic read-only prefixes.
    method.starts_with("web.get") || method.starts_with("web.read")
}
