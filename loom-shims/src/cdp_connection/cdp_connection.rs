// CdpConnection — raw WebSocket + JSON CDP multiplexer.
//
// # Contract semantics
// - **CDP wire format.** JSON-RPC over WebSocket. Each request is
//   `{"id": <u64>, "method": "<name>", "params": <object>}` for
//   browser-scope methods, or
//   `{"id": <u64>, "sessionId": "<sid>", "method": "<name>", "params": <object>}`
//   for page-scope methods. Each response is
//   `{"id": <u64>, "result": <object>}` or
//   `{"id": <u64>, "error": {"code": <int>, "message": "<text>"}}`. Async
//   events arrive as `{"method": "<name>", "params": <object>}` (no `id`),
//   optionally with a `sessionId` field for page-session events when
//   the connection has attached to a Page target.
//
// # Browser-scope vs page-scope (REAL CHROMIUM CONSTRAINT)
// The browser-level WS endpoint
// (`ws://127.0.0.1:<port>/devtools/browser/<uuid>`) only exposes
// `Browser.*`, `Target.*`, `Tracing.*`, `Storage.*`, `Schema.*`. To
// dispatch `Page.*`, `DOM.*`, `Network.*`, `Runtime.*`, etc., the
// connection must FIRST:
//   1. `Target.createTarget {url:"about:blank"}` → returns `targetId`.
//   2. `Target.attachToTarget {targetId, flatten:true}` → returns `sessionId`.
//   3. Send subsequent page-scope commands with a top-level `sessionId`
//      field on each request — otherwise real Chromium returns
//      `{code:-32601, message:"'<method>' wasn't found"}`.
// `connect()` performs this bootstrap automatically (see
// `bootstrap_page_session` below).
// - **Multiplex outbound commands.** Each `command(target_id, msg)`
//   allocates a unique `id` and parks a oneshot listener until the matching
//   response arrives. Multiple in-flight commands do NOT serialise
//   so cdp_send latency is bounded by the slowest in-flight command, not the queue.
// - **Demux to handlers via Arc<dyn Fn>, not static deps.**
//   `NetworkInterceptor` and `DeterminismInjector` register event
//   handlers at construction; `CdpConnection` does NOT import them.
// - **Crash invalidation hook.** `Supervisor::handle_crash` calls
//   `invalidate_session()` to drop the WebSocket. Subsequent commands
//   resolve to `CdpError::ConnectionClosed`.
// - **Action timeout.** Default 30s; overridable per-command. Timeouts
//   detach the pending oneshot so the writer task doesn't leak.
//
// # Why raw WS+JSON instead of chromiumoxide typed methods
// chromiumoxide 0.6 only exposes `Page::execute<T: Method>` for typed
// methods; there is no escape hatch for arbitrary `(method_name, params)`
// pairs. Building typed wrappers for every CDP method we'll need is
// strictly more code than running the JSON wire ourselves. The shim
// is the only crate allowed to link `chromiumoxide`
// (cargo-deny enforced) — that invariant is preserved either way.

use crate::cdp_connection::cbor_json::{cbor_to_json, json_to_cbor};
use crate::ipc_endpoint::ipc_endpoint::{CdpMessage, ShimErrorCode, TargetId};
use async_trait::async_trait;
use ciborium::value::Value as CborValue;
use dashmap::DashMap;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value as JsonValue};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

/// Default per-CDP-command timeout. Overridable per-call.
pub const DEFAULT_CDP_TIMEOUT: Duration = Duration::from_secs(30);

/// Event-handler closure type. Two are registered:
/// - `NetworkInterceptor` for `Network.*` events.
/// - `DeterminismInjector` for `Page.*` events.
pub type EventHandler = Arc<dyn Fn(TargetId, CdpMessage) + Send + Sync>;

/// Filter applied to events before invoking a handler.
#[derive(Debug, Clone)]
pub struct EventFilter {
    pub method_prefix: String,
}

impl EventFilter {
    pub fn new(method_prefix: impl Into<String>) -> Self {
        Self {
            method_prefix: method_prefix.into(),
        }
    }
}

/// In-flight request bookkeeping shared between command() and the reader task.
type PendingMap = Arc<DashMap<u64, oneshot::Sender<Result<JsonValue, String>>>>;
/// `(handler_id, filter, handler)` — the stable id is what lets an
/// [`EventRegistration`] remove exactly its own entry on drop.
type HandlerEntries = parking_lot::RwLock<Vec<(u64, EventFilter, EventHandler)>>;
type HandlerList = Arc<HandlerEntries>;

/// Live WS-backed connection state. Held inside `Mutex<Option<...>>` so
/// `invalidate_session()` can drop it cleanly.
struct CdpConnState {
    send_tx: mpsc::Sender<String>,
    pending: PendingMap,
    next_id: Arc<AtomicU64>,
    reader: JoinHandle<()>,
    writer: JoinHandle<()>,
    /// CDP session id from `Target.attachToTarget` (flatten:true). Set
    /// after `bootstrap_page_session` completes during connect; required
    /// on every page-scope request envelope. None until bootstrap finishes
    /// (page-scope commands fail-fast with TargetNotAttached in that
    /// window — only happens during the first ~50ms of the connect path).
    page_session_id: Option<String>,
    /// targetId from `Target.createTarget`. Stashed for diagnostics +
    /// future per-target operations (close, focus, etc.).
    #[allow(dead_code)]
    page_target_id: Option<String>,
}

impl CdpConnState {
    fn shutdown(self) {
        self.reader.abort();
        self.writer.abort();
        self.pending.clear();
    }
}

/// Concrete WS+JSON-backed connection.
pub struct ChromiumCdpConnection {
    inner: Arc<tokio::sync::Mutex<Option<CdpConnState>>>,
    handlers: HandlerList,
    /// Monotonic id source for handler registrations. A counter (not
    /// `handlers.len()`) so ids stay unique across removals.
    next_handler_id: AtomicU64,
    connected: Arc<AtomicBool>,
    pub(crate) default_timeout: Duration,
}

impl ChromiumCdpConnection {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(None)),
            handlers: Arc::new(parking_lot::RwLock::new(Vec::new())),
            next_handler_id: AtomicU64::new(0),
            connected: Arc::new(AtomicBool::new(false)),
            default_timeout: DEFAULT_CDP_TIMEOUT,
        }
    }
}

impl Default for ChromiumCdpConnection {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
pub trait CdpConnection: Send + Sync {
    async fn connect(&self, ws_url: &str) -> Result<(), CdpError>;

    async fn command(
        &self,
        target_id: TargetId,
        msg: CdpMessage,
        timeout: Option<Duration>,
    ) -> Result<CborValue, CdpError>;

    fn register_event_handler(
        &self,
        filter: EventFilter,
        handler: EventHandler,
    ) -> EventRegistration;

    fn invalidate_session(&self);

    fn is_connected(&self) -> bool;
}

/// RAII registration token. Dropping it removes the handler from the owning
/// connection's dispatch list — so per-navigate subscriptions (the load /
/// budget-expiry oneshots, the console collector, the settle driver's
/// `Network.` counter) are deregistered when their scope ends instead of
/// accumulating for the session's lifetime. Pre-RAII, every navigate leaked
/// up to 4 handlers: dispatch degraded to O(navigates) per event and each
/// orphaned console collector kept receiving (and retaining) every
/// subsequent console line — unbounded memory growth on chatty pages.
///
/// Long-lived subscribers (NetworkInterceptor, DeterminismInjector) keep
/// their token in a struct field, which preserves their handlers exactly
/// as before.
pub struct EventRegistration {
    pub handler_id: u64,
    /// Weak link back to the owning handler list. `None` for detached
    /// tokens (impls that don't support removal). Weak so a token held
    /// past `invalidate_session`/connection drop is a no-op, not a leak.
    list: Option<std::sync::Weak<HandlerEntries>>,
}

impl EventRegistration {
    /// Token that does NOT deregister on drop. For `CdpConnection` impls
    /// without a removable handler list.
    pub fn detached(handler_id: u64) -> Self {
        Self {
            handler_id,
            list: None,
        }
    }

    fn scoped(handler_id: u64, list: &HandlerList) -> Self {
        Self {
            handler_id,
            list: Some(Arc::downgrade(list)),
        }
    }
}

impl Drop for EventRegistration {
    fn drop(&mut self) {
        if let Some(weak) = self.list.take() {
            if let Some(list) = weak.upgrade() {
                list.write().retain(|(id, _, _)| *id != self.handler_id);
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CdpError {
    #[error("connection closed")]
    ConnectionClosed,
    #[error("CDP timeout after {ms} ms")]
    Timeout { ms: u64 },
    #[error("CDP protocol error: {0}")]
    Protocol(String),
    #[error("target not attached: {0}")]
    TargetNotAttached(TargetId),
    #[error("websocket connect failed: {0}")]
    ConnectFailed(String),
}

impl From<CdpError> for ShimErrorCode {
    fn from(e: CdpError) -> Self {
        match e {
            CdpError::ConnectionClosed => ShimErrorCode::ChromiumUnavailable,
            CdpError::Timeout { .. } => ShimErrorCode::CdpTimeout,
            CdpError::Protocol(_) => ShimErrorCode::CdpProtocolError,
            CdpError::TargetNotAttached(_) => ShimErrorCode::TargetUnknown,
            CdpError::ConnectFailed(_) => ShimErrorCode::ChromiumUnavailable,
        }
    }
}

#[async_trait]
impl CdpConnection for ChromiumCdpConnection {
    async fn connect(&self, ws_url: &str) -> Result<(), CdpError> {
        let mut guard = self.inner.lock().await;
        if guard.is_some() {
            return Ok(());
        }
        let (ws, _resp) = tokio_tungstenite::connect_async(ws_url)
            .await
            .map_err(|e| CdpError::ConnectFailed(e.to_string()))?;
        let (mut sink, mut stream) = ws.split();

        let (send_tx, mut send_rx) = mpsc::channel::<String>(64);
        let pending: PendingMap = Arc::new(DashMap::new());
        let next_id = Arc::new(AtomicU64::new(1));

        let writer = tokio::spawn(async move {
            while let Some(text) = send_rx.recv().await {
                if let Err(e) = sink.send(Message::Text(text.into())).await {
                    tracing::error!("cdp ws send: {e}");
                    break;
                }
            }
            let _ = sink.close().await;
        });

        let pending_for_reader = pending.clone();
        let handlers_for_reader = self.handlers.clone();
        let connected_for_reader = self.connected.clone();
        let reader = tokio::spawn(async move {
            while let Some(msg) = stream.next().await {
                let msg = match msg {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::error!("cdp ws recv: {e}");
                        break;
                    }
                };
                let text = match msg {
                    Message::Text(t) => t,
                    Message::Close(_) => break,
                    Message::Ping(_) | Message::Pong(_) => continue,
                    _ => continue,
                };
                let value: JsonValue = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!("cdp json decode: {e}");
                        continue;
                    }
                };
                if let Some(id) = value.get("id").and_then(|v| v.as_u64()) {
                    if let Some((_, tx)) = pending_for_reader.remove(&id) {
                        if let Some(err) = value.get("error") {
                            let _ = tx.send(Err(err.to_string()));
                        } else {
                            let result = value.get("result").cloned().unwrap_or(JsonValue::Null);
                            let _ = tx.send(Ok(result));
                        }
                    }
                } else if let (Some(method), Some(params)) = (
                    value.get("method").and_then(|v| v.as_str()),
                    value.get("params"),
                ) {
                    let handlers = handlers_for_reader.read();
                    for (_id, filter, handler) in handlers.iter() {
                        if method.starts_with(&filter.method_prefix) {
                            let cbor_params = json_to_cbor(params);
                            let cdp_msg = CdpMessage {
                                method: method.to_string(),
                                params: cbor_params,
                            };
                            handler(0, cdp_msg);
                        }
                    }
                }
            }
            // Connection closed — abort all pending and flip the flag.
            pending_for_reader.clear();
            connected_for_reader.store(false, Ordering::SeqCst);
        });

        *guard = Some(CdpConnState {
            send_tx,
            pending,
            next_id,
            reader,
            writer,
            page_session_id: None,
            page_target_id: None,
        });
        self.connected.store(true, Ordering::SeqCst);
        // Drop the lock before bootstrap so command_inner can re-acquire
        // it. bootstrap_page_session sends real CDP commands through the
        // same path as user commands.
        drop(guard);
        if let Err(e) = self.bootstrap_page_session().await {
            // Bootstrap failed AFTER the conn state was stored and
            // `connected` flipped true. Without cleanup the connection is
            // wedged half-open: is_connected() reports healthy, a retry of
            // connect() hits the `guard.is_some()` fast-path above and
            // returns Ok with page_session_id still None — after which
            // every page-scope command fails with TargetNotAttached. Tear
            // the state down so a retry re-runs the FULL connect path.
            //
            // Deliberately NOT invalidate_session(): that also clears the
            // handler list, and registrations made before connect (the
            // NetworkInterceptor / DeterminismInjector constructor-time
            // subscriptions) must survive a failed attempt so the retry's
            // reader keeps dispatching to them.
            self.connected.store(false, Ordering::SeqCst);
            let mut guard = self.inner.lock().await;
            if let Some(state) = guard.take() {
                state.shutdown();
            }
            return Err(e);
        }
        Ok(())
    }

    async fn command(
        &self,
        _target_id: TargetId,
        msg: CdpMessage,
        timeout: Option<Duration>,
    ) -> Result<CborValue, CdpError> {
        let timeout = timeout.unwrap_or(self.default_timeout);
        let (send_tx, pending, id, session_id) = {
            let guard = self.inner.lock().await;
            let Some(state) = guard.as_ref() else {
                return Err(CdpError::ConnectionClosed);
            };
            let id = state.next_id.fetch_add(1, Ordering::SeqCst);
            // Page-scope methods MUST carry the cached page_session_id
            // (post-`Target.attachToTarget`). Browser-scope methods
            // (Browser.*, Target.*, Tracing.*, Storage.*, Schema.*) MUST
            // NOT carry it — Chromium would respond with
            // `{code:-32602, message:"'sessionId' is unexpected"}` on
            // those. The classifier below picks the right wire shape.
            let session_id = if is_browser_scope_method(&msg.method) {
                None
            } else {
                state.page_session_id.clone()
            };
            (state.send_tx.clone(), state.pending.clone(), id, session_id)
        };

        // Page-scope command issued before bootstrap completed → fail-fast
        // rather than letting Chromium reject with -32601. This window is
        // ~50ms during connect; only racy if a caller invokes command()
        // concurrently with connect(), which the supervisor doesn't do.
        if !is_browser_scope_method(&msg.method) && session_id.is_none() {
            return Err(CdpError::TargetNotAttached(0));
        }

        let params_json = cbor_to_json(&msg.params);
        let mut request = json!({
            "id": id,
            "method": msg.method,
            "params": params_json,
        });
        if let Some(sid) = session_id {
            request["sessionId"] = json!(sid);
        }
        let request_text = request.to_string();

        let (resp_tx, resp_rx) = oneshot::channel();
        pending.insert(id, resp_tx);

        if send_tx.send(request_text).await.is_err() {
            pending.remove(&id);
            return Err(CdpError::ConnectionClosed);
        }

        match tokio::time::timeout(timeout, resp_rx).await {
            Ok(Ok(Ok(value))) => Ok(json_to_cbor(&value)),
            Ok(Ok(Err(detail))) => Err(CdpError::Protocol(detail)),
            Ok(Err(_)) => Err(CdpError::ConnectionClosed),
            Err(_) => {
                pending.remove(&id);
                Err(CdpError::Timeout {
                    ms: timeout.as_millis() as u64,
                })
            }
        }
    }

    fn register_event_handler(
        &self,
        filter: EventFilter,
        handler: EventHandler,
    ) -> EventRegistration {
        let id = self.next_handler_id.fetch_add(1, Ordering::SeqCst);
        self.handlers.write().push((id, filter, handler));
        EventRegistration::scoped(id, &self.handlers)
    }

    fn invalidate_session(&self) {
        // Try to take the inner state without blocking the runtime; if
        // the lock is contended (a command() is mid-flight), spawn a
        // fire-and-forget task to clear it asynchronously. Either way,
        // flip the connected flag immediately so subsequent command()
        // calls fast-fail.
        self.connected.store(false, Ordering::SeqCst);
        if let Ok(mut guard) = self.inner.try_lock() {
            if let Some(state) = guard.take() {
                state.shutdown();
            }
        } else {
            let inner = self.inner.clone();
            tokio::spawn(async move {
                let mut guard = inner.lock().await;
                if let Some(state) = guard.take() {
                    state.shutdown();
                }
            });
        }
        self.handlers.write().clear();
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}

/// Pure helper: does a method string match the filter? Used by tests +
/// the event-dispatch path.
pub fn method_matches(filter: &EventFilter, method: &str) -> bool {
    method.starts_with(&filter.method_prefix)
}

/// Classifier: which CDP method names are dispatched at the browser
/// scope (no sessionId required) vs. the page scope (sessionId required).
/// See the module-level "Browser-scope vs page-scope" doc-comment.
///
/// Browser-scope domains as of CDP `tot`:
///   Browser, Target, Tracing, Storage, Schema, SystemInfo, Memory,
///   PerformanceTimeline (subset), HeapProfiler (browser-process only).
///
/// Anything not browser-scope is treated as page-scope. False positives
/// (sending sessionId to a method that doesn't need it) are harmless on
/// real Chromium per the CDP spec — Chromium silently accepts an
/// unexpected sessionId on broadcast methods. False negatives (missing
/// sessionId on a page-scope method) trigger the -32601 we are fixing.
pub fn is_browser_scope_method(method: &str) -> bool {
    matches!(
        method.split('.').next().unwrap_or(""),
        "Browser" | "Target" | "Tracing" | "Storage" | "Schema" | "SystemInfo" | "Memory"
    )
}

impl ChromiumCdpConnection {
    /// Bootstrap a Page session on top of the just-connected browser-level
    /// WebSocket. Real Chromium requires this sequence before any
    /// `Page.*`/`DOM.*`/`Network.*`/`Runtime.*` command will dispatch.
    ///
    /// 1. `Target.createTarget {url:"about:blank"}` → `targetId`.
    /// 2. `Target.attachToTarget {targetId, flatten:true}` → `sessionId`.
    /// 3. Stash both on `CdpConnState` so subsequent `command()` calls
    ///    include `sessionId` on the wire.
    /// 4. `Page.enable` + `Network.enable` (page-scope, with sessionId)
    ///    so the `Page.loadEventFired` / `Network.responseReceived`
    ///    events the action_executor subscribes to actually fire.
    ///
    /// Errors propagate as `CdpError::ConnectFailed` (during bootstrap
    /// the connection is logically still in the connect path; the user-
    /// facing failure mode is "WebSocket connect failed").
    async fn bootstrap_page_session(&self) -> Result<(), CdpError> {
        // STEP 1: Target.createTarget. This is a browser-scope method, so
        // sessionId is intentionally omitted.
        let create_target = self
            .raw_command(
                "Target.createTarget",
                json!({"url": "about:blank"}),
                Duration::from_secs(10),
            )
            .await?;
        let target_id = create_target
            .get("targetId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                CdpError::ConnectFailed(format!(
                    "Target.createTarget returned no targetId: {create_target}"
                ))
            })?
            .to_string();

        // STEP 2: Target.attachToTarget. Browser-scope. flatten:true makes
        // subsequent commands route via top-level `sessionId` field
        // instead of the legacy `Target.sendMessageToTarget` envelope.
        let attach = self
            .raw_command(
                "Target.attachToTarget",
                json!({"targetId": target_id, "flatten": true}),
                Duration::from_secs(10),
            )
            .await?;
        let session_id = attach
            .get("sessionId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                CdpError::ConnectFailed(format!(
                    "Target.attachToTarget returned no sessionId: {attach}"
                ))
            })?
            .to_string();

        // STEP 3: stash on the connection state. Subsequent command()
        // calls pull session_id from here.
        {
            let mut guard = self.inner.lock().await;
            if let Some(state) = guard.as_mut() {
                state.page_session_id = Some(session_id.clone());
                state.page_target_id = Some(target_id);
            } else {
                return Err(CdpError::ConnectFailed(
                    "connection state dropped during bootstrap".into(),
                ));
            }
        }

        // STEP 4: enable the CDP domains that the action_executor subscribes to.
        // These are page-scope (now that we have a sessionId). Failures here
        // mean the navigate sequence won't see Page.loadEventFired or
        // Network.responseReceived events, which the action_executor relies
        // on. Surface as ConnectFailed so the supervisor's start path
        // catches it.
        //
        // Runtime.enable is required for Runtime.consoleAPICalled events to
        // fire. Without it the action_executor's navigate-time
        // console-collector subscribes to a handler that never receives
        // anything, and the receipt's console_count + console_lines come
        // back as 0 / empty even on pages that emit substantial console
        // output (e.g. React StrictMode warnings, Next.js dev hints).
        for (method, params) in [
            ("Page.enable", json!({})),
            ("Network.enable", json!({})),
            ("Runtime.enable", json!({})),
        ] {
            self.raw_command(method, params, Duration::from_secs(10))
                .await
                .map_err(|e| match e {
                    CdpError::Protocol(detail) => {
                        CdpError::ConnectFailed(format!("{method} during bootstrap: {detail}"))
                    }
                    other => other,
                })?;
        }

        // STEP 5: apply safe-profile download
        // confinement. When the daemon spawned this shim with
        // `LOOM_SHIM_PROFILE=safe` + `LOOM_SHIM_DOWNLOADS_DIR=<dir>`, send
        // `Browser.setDownloadBehavior(allowAndName, downloadPath=<dir>)`
        // so Chromium itself confines every download to the session-scoped
        // dir. Any attempt to save outside the dir is impossible by
        // construction (Chromium chooses the path, ignoring `<a download="...">`
        // attributes).
        //
        // Browser-scope CDP method — `is_browser_scope_method("Browser.*")`
        // returns true so `raw_command` correctly omits sessionId.
        //
        // Best-effort: a setDownloadBehavior failure does NOT block bootstrap.
        // The daemon-layer evaluate gate still fires for the
        // evaluate path, so the security model degrades gracefully — only
        // download confinement is impaired. Logged via tracing::warn so an
        // operator can spot it in the shim logs.
        if std::env::var("LOOM_SHIM_PROFILE").as_deref() == Ok("safe") {
            if let Ok(downloads_dir) = std::env::var("LOOM_SHIM_DOWNLOADS_DIR") {
                let result = self
                    .raw_command(
                        "Browser.setDownloadBehavior",
                        json!({
                            "behavior": "allowAndName",
                            "downloadPath": downloads_dir,
                        }),
                        Duration::from_secs(10),
                    )
                    .await;
                match result {
                    Ok(_) => {
                        tracing::info!(
                            downloads_dir = %downloads_dir,
                            "safe-profile: Browser.setDownloadBehavior(allowAndName) applied"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = ?e,
                            downloads_dir = %downloads_dir,
                            "safe-profile: Browser.setDownloadBehavior failed; downloads not confined"
                        );
                    }
                }
            } else {
                tracing::warn!(
                    "safe-profile: LOOM_SHIM_PROFILE=safe but LOOM_SHIM_DOWNLOADS_DIR unset — \
                     downloads NOT confined. Daemon should populate both env vars together."
                );
            }
        }

        Ok(())
    }

    /// Internal raw command dispatcher used by `bootstrap_page_session`.
    /// Goes through the same wire path as the public `command()` method
    /// (so the classifier + sessionId logic apply), but takes JSON params
    /// directly to avoid the round-trip through CBOR for the bootstrap
    /// values which are already JSON-shaped.
    async fn raw_command(
        &self,
        method: &str,
        params: JsonValue,
        timeout: Duration,
    ) -> Result<JsonValue, CdpError> {
        let (send_tx, pending, id, session_id) = {
            let guard = self.inner.lock().await;
            let Some(state) = guard.as_ref() else {
                return Err(CdpError::ConnectionClosed);
            };
            let id = state.next_id.fetch_add(1, Ordering::SeqCst);
            let session_id = if is_browser_scope_method(method) {
                None
            } else {
                state.page_session_id.clone()
            };
            (state.send_tx.clone(), state.pending.clone(), id, session_id)
        };

        let mut request = json!({
            "id": id,
            "method": method,
            "params": params,
        });
        if let Some(sid) = session_id {
            request["sessionId"] = json!(sid);
        }
        let request_text = request.to_string();

        let (resp_tx, resp_rx) = oneshot::channel();
        pending.insert(id, resp_tx);

        if send_tx.send(request_text).await.is_err() {
            pending.remove(&id);
            return Err(CdpError::ConnectionClosed);
        }

        match tokio::time::timeout(timeout, resp_rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(detail))) => Err(CdpError::Protocol(detail)),
            Ok(Err(_)) => Err(CdpError::ConnectionClosed),
            Err(_) => {
                pending.remove(&id);
                Err(CdpError::Timeout {
                    ms: timeout.as_millis() as u64,
                })
            }
        }
    }
}
