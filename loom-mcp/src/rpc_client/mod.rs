pub mod rpc_client;
pub use rpc_client::*;

#[cfg(test)]
mod interface_tests;

use crate::error_mapper::{DispatchPhase, ErrorMapper, McpContent, ToolResult};
use base64::Engine as _;
use loom_rpc::error::{LoomError, LoomErrorCode};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Max size of a screenshot we will inline as a base64 `image` content block on
/// a `tools/call` result. Larger images are NOT inlined (to bound the response
/// size); the client can still resolve them via the `loom://blob/<hash>`
/// resource. ~5 MiB comfortably covers full-page PNGs while capping the worst
/// case (council R1).
pub(crate) const MAX_INLINE_IMAGE_BYTES: usize = 5 * 1024 * 1024;

/// True iff `s` is exactly 64 lowercase/uppercase hex chars (a content-store
/// sha256). Guards the by-hash fetch + blob-resource URI against arbitrary
/// strings.
pub(crate) fn is_64_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

// ---------------------------------------------------------------------------
// Concrete framed-socket caller
// ---------------------------------------------------------------------------

struct FramedCaller {
    stream: tokio::sync::Mutex<loom_rpc::frame_handler::FramedUnixStream>,
    next_id: AtomicU64,
    /// Client-side per-call deadline (`RpcClientConfig::call_timeout`).
    call_timeout: Duration,
    /// Latched when a call hits the client-side deadline. The framed
    /// protocol has no response-id matching: the daemon's late reply to the
    /// timed-out request would be read as the reply to the NEXT request on
    /// this connection. Once set, every subsequent call fails fast with
    /// `TransportDropped` (PRE — nothing was sent) until `RpcClient::call`
    /// rebuilds the connection.
    dead: AtomicBool,
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
        // An earlier call on this connection hit the client-side deadline:
        // the connection is poisoned (see `dead` above) — fail fast so the
        // caller reconnects instead of mis-correlating frames.
        if self.dead.load(Ordering::SeqCst) {
            return Err(ErrorMapper::from_transport_dropped(
                "connection invalidated by an earlier timeout",
                DispatchPhase::Pre,
            ));
        }
        // Client-side deadline over the whole send+recv round trip. The
        // daemon enforces its own per-request deadline
        // (`LOOM_REQUEST_TIMEOUT_MS`, default 30 s) and answers with a typed
        // `request_timeout` envelope, so under normal operation the server
        // deadline wins this race; ours fires only when the daemon is wedged
        // (SIGSTOPped, deadlocked event loop) and can't even time itself
        // out — previously that hung the in-flight call, the keepalive
        // queued behind this stream mutex, and therefore the whole MCP
        // server, forever.
        let round_trip = tokio::time::timeout(self.call_timeout, async {
            // A send failure means the request never reached the daemon → PRE
            // dispatch → safe to retry on a fresh connection for any verb.
            stream
                .send(bytes::Bytes::from(req_bytes))
                .await
                .map_err(|_| {
                    ErrorMapper::from_transport_dropped(
                        "connection lost while sending",
                        DispatchPhase::Pre,
                    )
                })?;
            // The send completed; a read failure here means the request may
            // have been processed → POST dispatch → only idempotent verbs
            // auto-retry.
            stream
                .next()
                .await
                .ok_or_else(|| {
                    ErrorMapper::from_transport_dropped("connection closed", DispatchPhase::Post)
                })?
                .map_err(|_| {
                    ErrorMapper::from_transport_dropped("connection reset", DispatchPhase::Post)
                })
        })
        .await;
        let frame = match round_trip {
            // Deadline expired mid-round-trip: poison the connection and
            // surface a possibly-dispatched (POST) transport drop so the
            // reconnect/retry policy in `RpcClient::call` engages — the
            // connection is marked dead, torn down, and rebuilt on the next
            // attempt rather than blocked on.
            Err(_elapsed) => {
                self.dead.store(true, Ordering::SeqCst);
                return Err(ErrorMapper::from_transport_dropped(
                    "daemon unresponsive: client-side call deadline exceeded",
                    DispatchPhase::Post,
                ));
            }
            Ok(Err(e)) => return Err(e),
            Ok(Ok(frame)) => frame,
        };
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

/// Headroom added on top of the daemon's server-side per-request deadline to
/// form the default client-side `call_timeout`. Keeps the ordering invariant
/// `client deadline > server deadline`, so the daemon's typed
/// `request_timeout` envelope arrives first whenever the daemon is healthy
/// enough to produce one.
const CALL_TIMEOUT_HEADROOM_MS: u64 = 5_000;

/// The daemon's server-side per-request deadline in milliseconds. Mirrors
/// `loom-rpc::connection_handler::request_timeout` (`LOOM_REQUEST_TIMEOUT_MS`,
/// default 30 000) so an operator raising the daemon deadline via the shared
/// environment keeps the client deadline above it.
fn daemon_request_deadline_ms() -> u64 {
    std::env::var("LOOM_REQUEST_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(30_000)
}

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
            call_timeout: Duration::from_millis(
                daemon_request_deadline_ms().saturating_add(CALL_TIMEOUT_HEADROOM_MS),
            ),
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

    /// Test-only constructor: build a client already in the `Connected`
    /// state with `caller` as the injected transport, so unit tests can
    /// drive `call` / `call_as_tool_result` against canned RPC responses
    /// without a real Unix socket.
    #[cfg(test)]
    pub(crate) fn with_caller_for_test(caller: Box<dyn JsonRpcCaller + Send + Sync>) -> Arc<Self> {
        Arc::new(Self {
            state: Arc::new(tokio::sync::RwLock::new(ConnectionState::Connected)),
            inner: Arc::new(tokio::sync::RwLock::new(Some(RpcClientInner {
                handle: caller,
            }))),
            cfg: RpcClientConfig::defaults(),
            obs: crate::mcp_observability::McpObservability::new(true),
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
            call_timeout: self.cfg.call_timeout,
            dead: AtomicBool::new(false),
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
        self.call("health.ping", serde_json::json!({}))
            .await
            .map(|_| ())
    }

    pub async fn call_as_tool_result(
        self: &Arc<Self>,
        method: &str,
        params: serde_json::Value,
    ) -> ToolResult {
        let result = self.call(method, params).await;
        self.render_tool_result(method, result).await
    }

    /// Render an RPC outcome as an MCP `ToolResult`: text receipt block
    /// first (preserves the wire contract incl. screenshot_after_hash for
    /// all consumers), then an inline image block for screenshot-producing
    /// verbs. Split from `call_as_tool_result` so the dispatcher can
    /// intercept the typed error between call and render (implicit-session
    /// self-heal).
    pub async fn render_tool_result(
        self: &Arc<Self>,
        method: &str,
        result: Result<serde_json::Value, LoomError>,
    ) -> ToolResult {
        match result {
            Ok(v) => {
                let mut content = vec![McpContent::from_json(v.clone())];
                if let Some(img) = self.screenshot_image_block(method, &v).await {
                    content.push(img);
                }
                ToolResult {
                    is_error: false,
                    content,
                }
            }
            Err(e) => ErrorMapper::to_tool_result(e),
        }
    }

    /// Build an inline `McpContent::Image` for a screenshot-producing verb's
    /// receipt by resolving `screenshot_after_hash` through the content store
    /// (`content.get`). Returns `None` (and never errors the tool call) when the
    /// verb isn't a screenshot verb, the hash is absent/invalid, the blob can't
    /// be fetched, the bytes aren't a PNG, or the image exceeds the inline cap —
    /// in the oversize case the client can still fetch it via `loom://blob/<hash>`.
    async fn screenshot_image_block(
        self: &Arc<Self>,
        method: &str,
        value: &serde_json::Value,
    ) -> Option<McpContent> {
        if method != "web.screenshot" && method != "web.navigate" {
            return None;
        }
        let hash = value.get("screenshot_after_hash")?.as_str()?;
        let bytes = self.fetch_blob_by_hash(hash).await?;
        if bytes.len() > MAX_INLINE_IMAGE_BYTES {
            self.obs.info(
                "screenshot_image_skipped_oversize",
                serde_json::json!({ "hash": hash, "bytes": bytes.len() }),
            );
            return None;
        }
        if !loom_shared::screenshot_decode::is_png(&bytes) {
            return None;
        }
        Some(McpContent::Image {
            data: base64::engine::general_purpose::STANDARD.encode(&bytes),
            mime_type: "image/png".to_string(),
        })
    }

    /// Fetch raw bytes from the content store by 64-hex hash via the existing
    /// `content.get` RPC (which returns hex). Best-effort: logs and returns
    /// `None` on any failure so screenshot delivery degrades gracefully.
    pub(crate) async fn fetch_blob_by_hash(self: &Arc<Self>, hash: &str) -> Option<Vec<u8>> {
        if !is_64_hex(hash) {
            return None;
        }
        match self
            .call("content.get", serde_json::json!({ "artifact_ref": hash }))
            .await
        {
            Ok(v) => {
                let data_hex = v.get("data_hex").and_then(|d| d.as_str())?;
                match hex::decode(data_hex) {
                    Ok(bytes) => Some(bytes),
                    Err(_) => {
                        self.obs.info(
                            "screenshot_blob_bad_hex",
                            serde_json::json!({ "hash": hash }),
                        );
                        None
                    }
                }
            }
            Err(e) => {
                self.obs.info(
                    "screenshot_blob_fetch_failed",
                    serde_json::json!({ "hash": hash, "error": e.to_string() }),
                );
                None
            }
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
        "health.ping" | "daemon.health" | "session.list" | "session.info" | "session.close"
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
