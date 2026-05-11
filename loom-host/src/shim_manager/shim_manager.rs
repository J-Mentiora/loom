// ShimManager — out-of-process shim lifecycles.
//
// # Contract semantics
// - **The only direct out-of-process actor in `loom-host`.** All
//   chromium / ax interactions go through this module via
//   length-prefixed CBOR over `socketpair` for IPC framing.
// - **Pooled connections.** One `socketpair` per ShimId; the host
//   namespaces ShimId as `format!("{name}:{session_id}")` so each
//   session gets its own subprocess (reuse falls out naturally — a
//   second navigate hits the cached `ShimProcess`).
// - **Circuit breaker.** 3 consecutive spawn / IO / decode failures
//   opens the breaker for 5 s. Subsequent `send` calls fail-fast with
//   `ShimBreakerOpen` until the open window expires.
// - **Spawn-retry budget.** Single retry on initial spawn failure.
// - **No platform symbols here.** The shim binaries (chromium) contain
//   the platform-specific code; `ShimManager` only spawns and speaks
//   length-prefixed CBOR over the inherited socket FD.

use crate::host_observability::HostObservability;
use crate::shim_manager::process::{send_and_await, shutdown_process, ShimProcess, SpawnConfig};
use loom_core::error::{LoomError, LoomErrorCode};
use loom_shared::navigate_outcome::NavigateOutcome;
use loom_shared::shim_protocol::{ciborium_from_slice, CdpMessage, ShimRequest, ShimResponse};
use loom_shared::types::{EpochMs, Seed};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::task::JoinSet;

/// Logical shim id. Production keys are `format!("{name}:{session_id}")`
/// (e.g. `"chromium:01HXYZ..."`). Tests may use bare names.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
pub struct ShimId(pub String);

/// Spawn config per shim. Loaded from `HostConfig` once at startup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShimConfig {
    pub binary_path: PathBuf,
    pub args: Vec<String>,
    /// Environment variables to set on the child. The daemon populates
    /// `LOOM_SHIM_CHROMIUM_PATH` and `LOOM_SHIM_USER_DATA_DIR` here;
    /// `LOOM_SHIM_FD` is set automatically at spawn time.
    pub env: Vec<(String, String)>,
    pub spawn_retry: u8,       // soft default 1
    pub breaker_threshold: u8, // soft default 3
    pub breaker_open_ms: u64,  // soft default 5000
    pub send_timeout_ms: u64,
    pub recv_timeout_ms: u64,
}

impl Default for ShimConfig {
    fn default() -> Self {
        Self {
            binary_path: PathBuf::from("/usr/local/bin/loom-shim-stub"),
            args: vec![],
            env: vec![],
            spawn_retry: 1,
            breaker_threshold: 3,
            breaker_open_ms: 5_000,
            send_timeout_ms: 5_000,
            recv_timeout_ms: 30_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakerState {
    Closed,
    Open,
    HalfOpen,
}

/// Per-shim live state. Tracked by `ShimManager`.
pub struct ShimState {
    pub id: ShimId,
    pub breaker: BreakerState,
    pub consecutive_failures: u8,
    pub opened_at_ms: Option<u64>,
}

/// The manager.
pub struct ShimManager {
    pub(crate) configs: dashmap::DashMap<ShimId, ShimConfig>,
    pub(crate) states: dashmap::DashMap<ShimId, ShimState>,
    /// Live subprocess pool. One entry per `ShimId` that has a running
    /// child. Populated by lazy spawn on first `send`; drained by
    /// `shutdown_session` and on breaker-open evictions.
    pub(crate) processes: dashmap::DashMap<ShimId, Arc<ShimProcess>>,
    pub(crate) obs: Arc<HostObservability>,
    /// Allocates a stable u64 per host-side ULID for the wire's
    /// `session_id` field. Atomic counter avoids the hash-collision
    /// surface of a sha256/FxHash. Starts at 1 so 0 retains its
    /// "no real session" sentinel meaning.
    pub(crate) host_session_ids: dashmap::DashMap<String, u64>,
    pub(crate) host_session_counter: AtomicU64,
    /// Background cleanup tasks evicted from `processes` by
    /// `record_failure` (circuit-breaker eviction). The previous code
    /// used a fire-and-forget `tokio::spawn` here, which leaked
    /// `JoinHandle`s — when `shutdown_process` hung on SIGTERM grace
    /// each eviction left a never-joined task behind, and after a few
    /// sessions the daemon's runtime saturated. Now the spawn is owned
    /// by this `JoinSet` and reaped opportunistically on every
    /// `record_failure` and on every `shutdown_session`.
    pub(crate) cleanup_tasks: parking_lot::Mutex<JoinSet<()>>,
}

impl ShimManager {
    pub fn new(obs: Arc<HostObservability>) -> Arc<Self> {
        Arc::new(Self {
            configs: dashmap::DashMap::new(),
            states: dashmap::DashMap::new(),
            processes: dashmap::DashMap::new(),
            obs,
            host_session_ids: dashmap::DashMap::new(),
            host_session_counter: AtomicU64::new(0),
            cleanup_tasks: parking_lot::Mutex::new(JoinSet::new()),
        })
    }

    /// Map a host ULID to a stable u64 for the shim wire. Idempotent —
    /// calling twice with the same ULID returns the same u64. Threadsafe.
    pub fn shim_session_id_for(&self, ulid: &str) -> u64 {
        if let Some(v) = self.host_session_ids.get(ulid) {
            return *v;
        }
        let new = self.host_session_counter.fetch_add(1, Ordering::SeqCst) + 1;
        *self.host_session_ids.entry(ulid.to_string()).or_insert(new)
    }

    /// Register a shim's spawn config. Idempotent — replaces any
    /// existing entry for `id`.
    pub fn register(&self, id: ShimId, config: ShimConfig) {
        self.configs.insert(id, config);
    }

    /// Whether `id` has been registered.
    pub fn is_registered(&self, id: &ShimId) -> bool {
        self.configs.contains_key(id)
    }

    /// Send `msg` (opaque CBOR-encoded `CdpMessage` from the WASM guest)
    /// to `id` and await the response. Honors send/recv timeouts and
    /// the circuit breaker.
    pub async fn send(&self, id: ShimId, msg: Vec<u8>) -> Result<Vec<u8>, LoomError> {
        // Check circuit breaker.
        if let Some(state) = self.states.get(&id) {
            if state.breaker == BreakerState::Open {
                return Err(LoomError::new(
                    LoomErrorCode::ShimBreakerOpen,
                    format!("shim {} circuit breaker is open", id.0),
                ));
            }
        }

        // Decode the opaque CBOR bytes into a `CdpMessage`. The WASM
        // guest's cdp_message_encoder produces this shape.
        let cdp_msg: CdpMessage = match ciborium_from_slice(&msg) {
            Ok(m) => m,
            Err(e) => {
                self.record_failure(&id);
                return Err(LoomError::new(
                    LoomErrorCode::ShimFailure,
                    format!("shim {}: opaque payload not a CdpMessage: {e}", id.0),
                ));
            }
        };

        // Look up config.
        let config = self.configs.get(&id).map(|c| c.clone()).ok_or_else(|| {
            LoomError::new(
                LoomErrorCode::ShimFailure,
                format!("shim {} not registered", id.0),
            )
        })?;

        let process = self.get_or_spawn(&id, &config).await?;

        let request = ShimRequest::CdpSend {
            request_id: 0, // overwritten by send_and_await
            session_id: 0,
            target_id: 0,
            message: cdp_msg,
        };

        match send_and_await(
            &process,
            request,
            Duration::from_millis(config.send_timeout_ms),
            Duration::from_millis(config.recv_timeout_ms),
        )
        .await
        {
            Ok(ShimResponse::Ok { payload, .. }) => {
                self.record_success(&id);
                let mut bytes = Vec::new();
                if let Err(e) = ciborium::ser::into_writer(&payload, &mut bytes) {
                    return Err(LoomError::new(
                        LoomErrorCode::ShimFailure,
                        format!("re-encode response: {e}"),
                    ));
                }
                Ok(bytes)
            }
            Ok(ShimResponse::Error { code, detail, .. }) => {
                self.record_failure(&id);
                Err(LoomError::new(
                    map_shim_code(code),
                    format!("shim {}: {}", id.0, detail),
                ))
            }
            Ok(other) => {
                // CdpEvent / LogLine on the response oneshot is a protocol
                // violation — those go through the demux task's separate
                // path. If we got one here, the demux logic is buggy.
                self.record_failure(&id);
                Err(LoomError::new(
                    LoomErrorCode::ShimFailure,
                    format!("shim {}: unexpected non-Ok response: {other:?}", id.0),
                ))
            }
            Err(e) => {
                self.record_failure(&id);
                Err(e)
            }
        }
    }

    async fn get_or_spawn(
        &self,
        id: &ShimId,
        config: &ShimConfig,
    ) -> Result<Arc<ShimProcess>, LoomError> {
        if let Some(p) = self.processes.get(id) {
            return Ok(p.clone());
        }
        let spawn_config = SpawnConfig {
            binary_path: config.binary_path.clone(),
            args: config.args.clone(),
            env: config.env.clone(),
        };
        match crate::shim_manager::process::spawn_shim(&spawn_config).await {
            Ok(p) => {
                self.processes.insert(id.clone(), p.clone());
                Ok(p)
            }
            Err(e) => {
                self.record_failure(id);
                Err(e)
            }
        }
    }

    /// Send a typed PageNavigate request to `id` and decode the response
    /// as `NavigateOutcome`. Uses `ShimRequest::PageNavigate` (not CdpSend).
    /// `budget_ms` overrides the recv timeout when larger than the default.
    /// `action_id` is the WASM-guest-computed action hash (sha256 of the
    /// action payload); threaded for host-side receipt correlation /
    /// observability (Q5). The shim does not see action_id.
    /// `seed` and `epoch_ms` ride the wire to the shim where they're
    /// rendered into the determinism JS template at inject time.
    /// `blocklist_enabled` toggles the
    /// shim's `Fetch.enable` interception path; affirmative form on the
    /// wire so logs read directly.
    #[allow(clippy::too_many_arguments)]
    pub async fn send_navigate(
        &self,
        id: ShimId,
        action_id: String,
        session_id: u64,
        target_id: u64,
        url: String,
        budget_ms: u64,
        seed: Seed,
        epoch_ms: EpochMs,
        blocklist_enabled: bool,
    ) -> Result<NavigateOutcome, LoomError> {
        // action_id is reserved for receipt correlation (Q5 plumbing); not
        // sent to the shim — shim deals only with target_id + CDP frames.
        let _action_id = action_id;
        // Check circuit breaker.
        if let Some(state) = self.states.get(&id) {
            if state.breaker == BreakerState::Open {
                return Err(LoomError::new(
                    LoomErrorCode::ShimBreakerOpen,
                    format!("shim {} circuit breaker is open", id.0),
                ));
            }
        }

        let config = self.configs.get(&id).map(|c| c.clone()).ok_or_else(|| {
            LoomError::new(
                LoomErrorCode::ShimFailure,
                format!("shim {} not registered", id.0),
            )
        })?;

        let process = self.get_or_spawn(&id, &config).await?;

        let request = ShimRequest::PageNavigate {
            request_id: 0, // overwritten by send_and_await
            session_id,
            target_id,
            url,
            seed,
            epoch_ms,
            // Per-session toggle from `--no-blocklist`. Default `true`
            // (enforce); `false` when the operator opted out.
            blocklist_enabled,
        };

        // Use the larger of budget_ms and recv_timeout_ms so callers can
        // extend the timeout for slow pages without touching the config.
        let recv_ms = budget_ms.max(config.recv_timeout_ms);

        match send_and_await(
            &process,
            request,
            Duration::from_millis(config.send_timeout_ms),
            Duration::from_millis(recv_ms),
        )
        .await
        {
            Ok(ShimResponse::Ok { payload, .. }) => {
                self.record_success(&id);
                // Re-encode the ciborium Value to bytes so we can use
                // ciborium_from_slice → NavigateOutcome deserialization.
                // Field names in ActionResult::Navigated match NavigateOutcome
                // exactly; unknown fields (kind, target_id, frame_id, loader_id)
                // are silently ignored by serde.
                let mut bytes = Vec::new();
                if let Err(e) = ciborium::ser::into_writer(&payload, &mut bytes) {
                    return Err(LoomError::new(
                        LoomErrorCode::ShimFailure,
                        format!("shim {}: navigate response re-encode: {e}", id.0),
                    ));
                }
                ciborium_from_slice::<NavigateOutcome>(&bytes).map_err(|e| {
                    LoomError::new(
                        LoomErrorCode::ShimFailure,
                        format!("shim {}: navigate outcome decode: {e}", id.0),
                    )
                })
            }
            Ok(ShimResponse::Error { code, detail, .. }) => {
                self.record_failure(&id);
                Err(LoomError::new(
                    map_shim_code(code),
                    format!("shim {}: {}", id.0, detail),
                ))
            }
            Ok(other) => {
                self.record_failure(&id);
                Err(LoomError::new(
                    LoomErrorCode::ShimFailure,
                    format!("shim {}: unexpected non-Ok response: {other:?}", id.0),
                ))
            }
            Err(e) => {
                self.record_failure(&id);
                Err(e)
            }
        }
    }

    /// Send `Runtime.evaluate` against `id`'s target via CdpSend and parse
    /// the response into a typed `EvaluateOutcome`.
    /// `action_id` is the WASM-guest-computed action hash — threaded for
    /// receipt correlation; not sent to the shim.
    ///
    /// CDP `Runtime.evaluate` shape:
    ///   request:  {expression, returnByValue:true, awaitPromise:true}
    ///   response: { result: {type, value?}, exceptionDetails?: {...} }
    /// On exceptionDetails the host wraps as HostError::ShimFailure with
    /// `{kind:"js_throw", exception:..., line:..., column:...}` JSON.
    #[allow(clippy::too_many_arguments)]
    pub async fn send_evaluate(
        &self,
        id: ShimId,
        action_id: String,
        session_id: u64,
        target_id: u64,
        expression: String,
        budget_ms: u64,
        seed: Seed,
        epoch_ms: EpochMs,
    ) -> Result<EvaluateOutcome, LoomError> {
        // action_id reserved for receipt correlation (Q5 plumbing).
        let _action_id = action_id;

        if let Some(state) = self.states.get(&id) {
            if state.breaker == BreakerState::Open {
                return Err(LoomError::new(
                    LoomErrorCode::ShimBreakerOpen,
                    format!("shim {} circuit breaker is open", id.0),
                ));
            }
        }

        let config = self.configs.get(&id).map(|c| c.clone()).ok_or_else(|| {
            LoomError::new(
                LoomErrorCode::ShimFailure,
                format!("shim {} not registered", id.0),
            )
        })?;

        let process = self.get_or_spawn(&id, &config).await?;

        // Lazy-spawn the determinism-injected target before evaluating.
        // The shim's `CdpSend` handler does NOT do this on its own (only
        // `PageNavigate` does), so an evaluate-only flow would otherwise
        // route to the bootstrap about:blank context where Date.now /
        // Math.random still leak real wall-clock + unseeded values.
        // SpawnTarget is idempotent at the TargetManager level
        // so navigate-then-evaluate paths pay no extra cost.
        let spawn_request = ShimRequest::SpawnTarget {
            request_id: 0,
            session_id,
            profile: "default".to_string(),
            seed,
            epoch_ms,
        };
        // Best-effort: if SpawnTarget fails (e.g. unknown shim error),
        // fall through to the eval anyway and surface the eval's own
        // error path. The most common failure here is "target already
        // exists" which the dispatcher treats as Ok.
        let _ = send_and_await(
            &process,
            spawn_request,
            Duration::from_millis(config.send_timeout_ms),
            Duration::from_millis(config.recv_timeout_ms),
        )
        .await;

        // Build CDP Runtime.evaluate params as a CBOR map. `returnByValue`
        // gives us the value back as CBOR (vs. an opaque object handle);
        // `awaitPromise` resolves promises within the budget window.
        let params = ciborium::value::Value::Map(vec![
            (
                ciborium::value::Value::Text("expression".into()),
                ciborium::value::Value::Text(expression),
            ),
            (
                ciborium::value::Value::Text("returnByValue".into()),
                ciborium::value::Value::Bool(true),
            ),
            (
                ciborium::value::Value::Text("awaitPromise".into()),
                ciborium::value::Value::Bool(true),
            ),
        ]);

        let request = ShimRequest::CdpSend {
            request_id: 0,
            session_id,
            target_id,
            message: CdpMessage {
                method: "Runtime.evaluate".into(),
                params,
            },
        };

        let recv_ms = budget_ms.max(config.recv_timeout_ms);

        match send_and_await(
            &process,
            request,
            Duration::from_millis(config.send_timeout_ms),
            Duration::from_millis(recv_ms),
        )
        .await
        {
            Ok(ShimResponse::Ok { payload, .. }) => {
                self.record_success(&id);
                parse_evaluate_payload(&payload).map_err(|e| {
                    LoomError::new(
                        LoomErrorCode::ShimFailure,
                        format!("shim {}: evaluate response parse: {e}", id.0),
                    )
                })
            }
            Ok(ShimResponse::Error { code, detail, .. }) => {
                self.record_failure(&id);
                Err(LoomError::new(
                    map_shim_code(code),
                    format!("shim {}: {}", id.0, detail),
                ))
            }
            Ok(other) => {
                self.record_failure(&id);
                Err(LoomError::new(
                    LoomErrorCode::ShimFailure,
                    format!("shim {}: unexpected non-Ok response: {other:?}", id.0),
                ))
            }
            Err(e) => {
                self.record_failure(&id);
                Err(e)
            }
        }
    }

    /// Cooperatively shut down all shim subprocesses bound to
    /// `session_id` (matched on the `:<session_id>` suffix of the
    /// ShimId). Called by the daemon's session-close handler.
    pub async fn shutdown_session(&self, session_id: &str) {
        let suffix = format!(":{session_id}");
        let keys: Vec<ShimId> = self
            .processes
            .iter()
            .filter(|kv| kv.key().0.ends_with(&suffix))
            .map(|kv| kv.key().clone())
            .collect();
        for key in keys {
            if let Some((_, process)) = self.processes.remove(&key) {
                shutdown_process(process).await;
            }
            self.states.remove(&key);
            self.configs.remove(&key);
        }
        // Reap any completed breaker-eviction cleanup tasks. Lock release
        // happens at scope exit; JoinSet::try_join_next is non-blocking.
        let mut set = self.cleanup_tasks.lock();
        while set.try_join_next().is_some() {}
    }

    /// Increment the breaker counter on a failure. Opens the breaker
    /// at threshold and evicts the live process so the next call
    /// triggers a fresh spawn.
    fn record_failure(&self, id: &ShimId) {
        let threshold = self
            .configs
            .get(id)
            .map(|c| c.breaker_threshold)
            .unwrap_or(3);
        let mut state = self.states.entry(id.clone()).or_insert_with(|| ShimState {
            id: id.clone(),
            breaker: BreakerState::Closed,
            consecutive_failures: 0,
            opened_at_ms: None,
        });
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        if state.consecutive_failures >= threshold {
            state.breaker = BreakerState::Open;
            state.opened_at_ms = Some(now_ms());
        }
        // Evict the live process if any — the next call will half-open.
        drop(state);
        if let Some((_, p)) = self.processes.remove(id) {
            let mut set = self.cleanup_tasks.lock();
            set.spawn(shutdown_process(p));
            // Opportunistically reap completed cleanups so the JoinSet
            // doesn't grow unbounded across many breaker evictions.
            while set.try_join_next().is_some() {}
        }
    }

    fn record_success(&self, id: &ShimId) {
        if let Some(mut s) = self.states.get_mut(id) {
            s.consecutive_failures = 0;
            s.breaker = BreakerState::Closed;
            s.opened_at_ms = None;
        }
    }

    /// Snapshot the breaker state for diagnostics.
    /// Snapshot of every tracked shim's breaker state. Used by
    /// `daemon.health` to surface per-shim circuit-breaker visibility.
    /// Cheap iteration over the DashMap; no locks held across awaits.
    pub fn breaker_state_snapshot(&self) -> Vec<(ShimId, BreakerState, u8, Option<u64>)> {
        self.states
            .iter()
            .map(|kv| {
                let s = kv.value();
                (s.id.clone(), s.breaker, s.consecutive_failures, s.opened_at_ms)
            })
            .collect()
    }

    pub fn breaker_state(&self, id: &ShimId) -> Option<BreakerState> {
        self.states.get(id).map(|s| s.breaker)
    }

    /// Force-close the breaker (test seam + operator recovery).
    pub fn breaker_reset(&self, id: &ShimId) {
        if let Some(mut s) = self.states.get_mut(id) {
            s.breaker = BreakerState::Closed;
            s.consecutive_failures = 0;
            s.opened_at_ms = None;
        }
    }

    /// Soft-default open window. Used by tests + diagnostics.
    pub fn breaker_open_window(&self, id: &ShimId) -> Duration {
        self.configs
            .get(id)
            .map(|c| Duration::from_millis(c.breaker_open_ms))
            .unwrap_or(Duration::from_secs(5))
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn map_shim_code(code: loom_shared::shim_protocol::ShimErrorCode) -> LoomErrorCode {
    use loom_shared::shim_protocol::ShimErrorCode as E;
    match code {
        E::ChromiumUnavailable => LoomErrorCode::ShimFailure,
        E::CdpTimeout => LoomErrorCode::ShimTimeout,
        E::CdpProtocolError | E::TargetUnknown | E::ShimInternalError => LoomErrorCode::ShimFailure,
    }
}

// ─── Evaluate types ─────────────────────────────────────────────────────────

/// Parsed result of a `Runtime.evaluate` CDP call. Exactly one of `result`
/// / `exception` is `Some` per CDP semantics.
#[derive(Debug, Clone, PartialEq)]
pub struct EvaluateOutcome {
    /// On success: the raw CBOR `result.value`. None when the page threw.
    pub result: Option<ciborium::value::Value>,
    /// On exception: structured details. None on successful evaluation.
    pub exception: Option<EvaluateException>,
}

/// Structured page-side exception details from CDP `exceptionDetails`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluateException {
    /// `exceptionDetails.text` — usually `"Uncaught"`.
    pub text: String,
    /// Extracted exception message (`exception.description` or
    /// stringified `exception.value`). Used for the
    /// `details.exception` field on page-side throws.
    pub message: String,
    pub line: u32,
    pub column: u32,
}

/// Parse a CDP `Runtime.evaluate` response payload (CBOR map) into an
/// `EvaluateOutcome`. The response shape is documented at
/// https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-evaluate
fn parse_evaluate_payload(payload: &ciborium::value::Value) -> Result<EvaluateOutcome, String> {
    use ciborium::value::Value;

    let map = match payload {
        Value::Map(m) => m,
        other => return Err(format!("expected CBOR map, got {other:?}")),
    };

    let lookup = |key: &str| -> Option<&Value> {
        map.iter().find_map(|(k, v)| {
            if let Value::Text(s) = k {
                if s == key {
                    Some(v)
                } else {
                    None
                }
            } else {
                None
            }
        })
    };

    if let Some(ed) = lookup("exceptionDetails") {
        let ed_map = match ed {
            Value::Map(m) => m,
            _ => return Err("exceptionDetails not a map".into()),
        };
        let ed_lookup = |key: &str| -> Option<&Value> {
            ed_map.iter().find_map(|(k, v)| {
                if let Value::Text(s) = k {
                    if s == key {
                        Some(v)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
        };
        let text = match ed_lookup("text") {
            Some(Value::Text(s)) => s.clone(),
            _ => "Uncaught".into(),
        };
        let line = match ed_lookup("lineNumber") {
            Some(Value::Integer(i)) => u32::try_from(i128::from(*i)).unwrap_or(0),
            _ => 0,
        };
        let column = match ed_lookup("columnNumber") {
            Some(Value::Integer(i)) => u32::try_from(i128::from(*i)).unwrap_or(0),
            _ => 0,
        };
        // exception is a RemoteObject — pull description (preferred) or
        // value (fallback for primitive throws). Per CDP, `description`
        // carries the human-readable string for Error objects;
        // `value` carries the stringified primitive for `throw "x"`.
        let message = match ed_lookup("exception") {
            Some(Value::Map(em)) => {
                let em_lookup = |key: &str| -> Option<&Value> {
                    em.iter().find_map(|(k, v)| {
                        if let Value::Text(s) = k {
                            if s == key {
                                Some(v)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    })
                };
                if let Some(Value::Text(s)) = em_lookup("description") {
                    s.clone()
                } else if let Some(v) = em_lookup("value") {
                    stringify_cbor_primitive(v)
                } else {
                    text.clone()
                }
            }
            _ => text.clone(),
        };
        return Ok(EvaluateOutcome {
            result: None,
            exception: Some(EvaluateException {
                text,
                message,
                line,
                column,
            }),
        });
    }

    // Success path: result.value (or result.unserializableValue for things
    // like Infinity / NaN that don't survive CBOR. CDP places them in
    // `unserializableValue` as strings.)
    let result_obj = lookup("result")
        .ok_or_else(|| "evaluate response missing both result and exceptionDetails".to_string())?;
    let result_map = match result_obj {
        Value::Map(m) => m,
        _ => return Err("result not a map".into()),
    };
    let res_lookup = |key: &str| -> Option<&Value> {
        result_map.iter().find_map(|(k, v)| {
            if let Value::Text(s) = k {
                if s == key {
                    Some(v)
                } else {
                    None
                }
            } else {
                None
            }
        })
    };
    if let Some(v) = res_lookup("value") {
        return Ok(EvaluateOutcome {
            result: Some(v.clone()),
            exception: None,
        });
    }
    if let Some(Value::Text(s)) = res_lookup("unserializableValue") {
        // NaN, Infinity, -Infinity, -0, BigInt → CDP serializes as a
        // string. Surface as a Text value so cbor_value_to_json can
        // string-coerce per Q6.
        return Ok(EvaluateOutcome {
            result: Some(Value::Text(s.clone())),
            exception: None,
        });
    }
    // `evaluate('undefined')` returns { result: { type: "undefined" } }
    // with no `value`. Surface as Null per CDP convention.
    Ok(EvaluateOutcome {
        result: Some(Value::Null),
        exception: None,
    })
}

/// Stringify a primitive CBOR value for inclusion in an exception message.
fn stringify_cbor_primitive(v: &ciborium::value::Value) -> String {
    use ciborium::value::Value;
    match v {
        Value::Text(s) => s.clone(),
        Value::Integer(i) => format!("{}", i128::from(*i)),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".into(),
        other => format!("{other:?}"),
    }
}
