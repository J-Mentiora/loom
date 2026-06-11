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
// - **Circuit breaker.** 3 consecutive failures opens the breaker for
//   5 s (`breaker_open_ms`). Subsequent `send` calls fail-fast with
//   `ShimBreakerOpen` until the open window expires; the first call
//   after expiry transitions the breaker to `HalfOpen` and proceeds as
//   a recovery probe — success closes the breaker, failure re-opens it
//   with a fresh window. Failures are classified: transport failures
//   (spawn / IO / timeout — the subprocess or socket is unhealthy) also
//   evict the live subprocess; application failures (shim-REPORTED
//   errors from a live shim, e.g. a CDP protocol error) only count
//   toward the threshold and never kill a healthy Chromium.
// - **Spawn-retry budget.** Single retry on initial spawn failure.
// - **No platform symbols here.** The shim binaries (chromium) contain
//   the platform-specific code; `ShimManager` only spawns and speaks
//   length-prefixed CBOR over the inherited socket FD.

use crate::host_observability::HostObservability;
use crate::shim_manager::process::{send_and_await, shutdown_process, ShimProcess, SpawnConfig};
use loom_core::error::{LoomError, LoomErrorCode};
use loom_shared::navigate_outcome::{NavigateOutcome, NetworkLogOutcome};
use loom_shared::shim_protocol::{
    ciborium_from_slice, ciborium_to_vec, CdpMessage, ShimHealthInfo, ShimRequest, ShimResponse,
};
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

/// Failure classification for `record_failure`.
///
/// - `Transport`: the subprocess or its socket is unhealthy (spawn
///   failure, channel closed, send/recv timeout, crash, framing/demux
///   protocol violation). Counts toward the breaker AND evicts the live
///   process so the next admitted call respawns fresh.
/// - `Application`: a live, responsive shim REPORTED an error (e.g.
///   `CdpProtocolError` / `TargetUnknown` from a bad `Runtime.evaluate`),
///   or the host failed to decode the guest's payload before the shim
///   was ever involved. Counts toward the breaker threshold but does NOT
///   evict — killing the subprocess here would destroy a healthy
///   Chromium's mid-session browser state (current page, in-memory
///   state, in-flight navigation) over an error the browser survived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureClass {
    Transport,
    Application,
}

/// Per-shim live state. Tracked by `ShimManager`.
pub struct ShimState {
    pub id: ShimId,
    pub breaker: BreakerState,
    pub consecutive_failures: u8,
    pub opened_at_ms: Option<u64>,
    /// Lifecycle counters. Lives on `ShimState` (not `ShimProcess`) because
    /// `ShimProcess` is replaced on every respawn; these counters persist
    /// across the replacement so the operator can see "this shim has been
    /// restarted N times" via `daemon.health({deep:true})`.
    pub restart_count: u32,
    pub last_restart_at_ms: Option<u64>,
}

/// Owned snapshot of `ShimState` for callers that need to read it outside
/// the DashMap guard (e.g. async aggregators that can't hold a Ref across
/// `.await`). Mirror of the live struct.
#[derive(Debug, Clone)]
pub struct ShimStateSnapshot {
    pub id: ShimId,
    pub breaker: BreakerState,
    pub consecutive_failures: u8,
    pub opened_at_ms: Option<u64>,
    pub restart_count: u32,
    pub last_restart_at_ms: Option<u64>,
}

/// The manager.
pub struct ShimManager {
    pub(crate) configs: dashmap::DashMap<ShimId, ShimConfig>,
    pub(crate) states: dashmap::DashMap<ShimId, ShimState>,
    /// Live subprocess pool. One entry per `ShimId` that has a running
    /// child. Populated by lazy spawn on first `send`; drained by
    /// `shutdown_session` and on transport-failure evictions.
    pub(crate) processes: dashmap::DashMap<ShimId, Arc<ShimProcess>>,
    /// Per-id spawn mutual exclusion. Without this, two concurrent first
    /// calls for the same `ShimId` both miss the `processes` cache and
    /// both spawn — the second insert displaces the first Arc, leaving a
    /// transient untracked shim sharing the same `--user-data-dir`
    /// (split-brain + spurious first-action errors). The lock makes
    /// check-spawn-insert atomic per id; losers of the race await the
    /// winner and get the same `Arc<ShimProcess>`.
    pub(crate) spawn_locks: dashmap::DashMap<ShimId, Arc<tokio::sync::Mutex<()>>>,
    pub(crate) obs: Arc<HostObservability>,
    /// Allocates a stable u64 per host-side ULID for the wire's
    /// `session_id` field. Atomic counter avoids the hash-collision
    /// surface of a sha256/FxHash. Starts at 1 so 0 retains its
    /// "no real session" sentinel meaning.
    pub(crate) host_session_ids: dashmap::DashMap<String, u64>,
    pub(crate) host_session_counter: AtomicU64,
    /// Background cleanup tasks evicted from `processes` by
    /// `record_failure` (transport-failure eviction). The previous code
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
            spawn_locks: dashmap::DashMap::new(),
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
        self.check_breaker(&id)?;

        // Decode the opaque CBOR bytes into a `CdpMessage`. The WASM
        // guest's cdp_message_encoder produces this shape. A decode
        // failure here is the HOST's problem (bad guest payload) — the
        // shim did nothing wrong, so it counts as an application failure
        // and must not kill the subprocess.
        let cdp_msg: CdpMessage = match ciborium_from_slice(&msg) {
            Ok(m) => m,
            Err(e) => {
                self.record_failure(&id, FailureClass::Application);
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
                self.record_failure(&id, shim_error_class(&code));
                Err(LoomError::new(
                    map_shim_code(code),
                    format!("shim {}: {}", id.0, detail),
                ))
            }
            Ok(other) => {
                // CdpEvent / LogLine on the response oneshot is a protocol
                // violation — those go through the demux task's separate
                // path. If we got one here, the demux logic is buggy.
                self.record_failure(&id, FailureClass::Transport);
                Err(LoomError::new(
                    LoomErrorCode::ShimFailure,
                    format!("shim {}: unexpected non-Ok response: {other:?}", id.0),
                ))
            }
            Err(e) => {
                self.record_failure(&id, FailureClass::Transport);
                Err(e)
            }
        }
    }

    pub(crate) async fn get_or_spawn(
        &self,
        id: &ShimId,
        config: &ShimConfig,
    ) -> Result<Arc<ShimProcess>, LoomError> {
        // Fast path: hand back a cached, live shim without touching the
        // spawn lock.
        if let Some(p) = self.processes.get(id) {
            if !p.crashed.load(Ordering::SeqCst) {
                return Ok(p.clone());
            }
        }

        // Slow path: per-id mutual exclusion around check-spawn-insert.
        // Clone the Arc out of the entry guard before awaiting so no
        // DashMap shard lock is held across the `.await`.
        let spawn_lock = self
            .spawn_locks
            .entry(id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _guard = spawn_lock.lock().await;

        // Re-check under the lock: a concurrent caller may have spawned
        // while we waited. Proactive liveness check: only hand back a
        // cached shim if it is still alive. A crashed chromium/CDP shim
        // that's still in the map would otherwise be returned as a dead
        // handle, so every session created after a browser crash inherits
        // the dead browser (the reported "sessions after a crash get a
        // dead browser" bug). Evict the corpse and fall through to
        // respawn. The breaker (checked by callers before reaching here)
        // bounds repeated crash-respawn churn.
        let cached_dead = {
            if let Some(p) = self.processes.get(id) {
                if !p.crashed.load(Ordering::SeqCst) {
                    return Ok(p.clone());
                }
                true
            } else {
                false
            }
        };
        if cached_dead {
            // Drop the dead handle (its watcher already reaped the OS process);
            // remove() is a no-op if a concurrent caller beat us to it.
            self.processes.remove(id);
            tracing::warn!(shim = %id.0, "evicting crashed shim before reuse; respawning");
        }
        let spawn_config = SpawnConfig {
            binary_path: config.binary_path.clone(),
            args: config.args.clone(),
            env: config.env.clone(),
        };
        // Pre-spawn snapshot: did a state entry already exist? If yes the
        // upcoming spawn is a respawn, not a first spawn. Gating on this
        // avoids overcounting in the open-breaker path (record_failure
        // creates the state entry but the next call would otherwise be
        // rejected by the breaker before reaching here).
        let is_respawn = self.states.contains_key(id);
        match crate::shim_manager::process::spawn_shim(&spawn_config).await {
            Ok(p) => {
                self.processes.insert(id.clone(), p.clone());
                if is_respawn {
                    // Bump restart bookkeeping; create the entry first if
                    // somehow missing (record_failure should have created it
                    // but guard for the corner case).
                    let mut s = self.states.entry(id.clone()).or_insert_with(|| ShimState {
                        id: id.clone(),
                        breaker: BreakerState::Closed,
                        consecutive_failures: 0,
                        opened_at_ms: None,
                        restart_count: 0,
                        last_restart_at_ms: None,
                    });
                    s.restart_count = s.restart_count.saturating_add(1);
                    s.last_restart_at_ms = Some(now_ms());
                }
                Ok(p)
            }
            Err(e) => {
                self.record_failure(id, FailureClass::Transport);
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
        until: String,
        determinism_enabled: bool,
    ) -> Result<NavigateOutcome, LoomError> {
        // action_id is reserved for receipt correlation (Q5 plumbing); not
        // sent to the shim — shim deals only with target_id + CDP frames.
        let _action_id = action_id;
        self.check_breaker(&id)?;

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
            // settle-capture: readiness mode gating the capture.
            until,
            // settle-capture (4b): per-session determinism toggle.
            determinism_enabled,
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
                self.record_failure(&id, shim_error_class(&code));
                Err(LoomError::new(
                    map_shim_code(code),
                    format!("shim {}: {}", id.0, detail),
                ))
            }
            Ok(other) => {
                self.record_failure(&id, FailureClass::Transport);
                Err(LoomError::new(
                    LoomErrorCode::ShimFailure,
                    format!("shim {}: unexpected non-Ok response: {other:?}", id.0),
                ))
            }
            Err(e) => {
                self.record_failure(&id, FailureClass::Transport);
                Err(e)
            }
        }
    }

    /// Read the shim's full-capture network-entries accumulator (everything
    /// observed since the last navigate). Observation-only — no CDP round-trip,
    /// no navigate. Backs the `loom.web.network_log` tool.
    pub async fn send_network_log(
        &self,
        id: ShimId,
        session_id: u64,
        target_id: u64,
    ) -> Result<NetworkLogOutcome, LoomError> {
        self.check_breaker(&id)?;
        let config = self.configs.get(&id).map(|c| c.clone()).ok_or_else(|| {
            LoomError::new(
                LoomErrorCode::ShimFailure,
                format!("shim {} not registered", id.0),
            )
        })?;
        let process = self.get_or_spawn(&id, &config).await?;
        let request = ShimRequest::GetNetworkLog {
            request_id: 0, // overwritten by send_and_await
            session_id,
            target_id,
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
                        format!("shim {}: network_log response re-encode: {e}", id.0),
                    ));
                }
                ciborium_from_slice::<NetworkLogOutcome>(&bytes).map_err(|e| {
                    LoomError::new(
                        LoomErrorCode::ShimFailure,
                        format!("shim {}: network_log outcome decode: {e}", id.0),
                    )
                })
            }
            Ok(ShimResponse::Error { code, detail, .. }) => {
                self.record_failure(&id, shim_error_class(&code));
                Err(LoomError::new(
                    map_shim_code(code),
                    format!("shim {}: {}", id.0, detail),
                ))
            }
            Ok(other) => {
                self.record_failure(&id, FailureClass::Transport);
                Err(LoomError::new(
                    LoomErrorCode::ShimFailure,
                    format!("shim {}: unexpected non-Ok response: {other:?}", id.0),
                ))
            }
            Err(e) => {
                self.record_failure(&id, FailureClass::Transport);
                Err(e)
            }
        }
    }

    /// settle-capture slice 2: run a standalone readiness wait on the session's
    /// current target via `ShimRequest::WaitFor`, parsing the response into a
    /// typed `WaitOutcome`. Mirrors `send_evaluate`: an idempotent SpawnTarget
    /// first so the wait runs against the determinism-injected target (not the
    /// bootstrap about:blank), then the typed wait request.
    #[allow(clippy::too_many_arguments)]
    pub async fn send_wait_for(
        &self,
        id: ShimId,
        action_id: String,
        session_id: u64,
        target_id: u64,
        until: String,
        budget_ms: u64,
        seed: Seed,
        epoch_ms: EpochMs,
        determinism_enabled: bool,
    ) -> Result<loom_shared::navigate_outcome::WaitOutcome, LoomError> {
        // action_id reserved for receipt correlation (Q5 plumbing).
        let _action_id = action_id;

        self.check_breaker(&id)?;

        let config = self.configs.get(&id).map(|c| c.clone()).ok_or_else(|| {
            LoomError::new(
                LoomErrorCode::ShimFailure,
                format!("shim {} not registered", id.0),
            )
        })?;

        let process = self.get_or_spawn(&id, &config).await?;

        // Idempotent lazy-spawn (same rationale as send_evaluate): ensures the
        // wait runs against the seeded target, never the about:blank bootstrap.
        let spawn_request = ShimRequest::SpawnTarget {
            request_id: 0,
            session_id,
            profile: "default".to_string(),
            seed,
            epoch_ms,
            // settle-capture (4b): per-session determinism toggle.
            determinism_enabled,
        };
        let _ = send_and_await(
            &process,
            spawn_request,
            Duration::from_millis(config.send_timeout_ms),
            Duration::from_millis(config.recv_timeout_ms),
        )
        .await;

        let request = ShimRequest::WaitFor {
            request_id: 0,
            session_id,
            target_id,
            until,
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
                // Re-encode the ciborium Value, then decode as WaitOutcome.
                // Field names in ActionResult::Waited match WaitOutcome; the
                // `kind` tag is ignored by serde.
                let mut bytes = Vec::new();
                if let Err(e) = ciborium::ser::into_writer(&payload, &mut bytes) {
                    return Err(LoomError::new(
                        LoomErrorCode::ShimFailure,
                        format!("shim {}: wait_for response re-encode: {e}", id.0),
                    ));
                }
                ciborium_from_slice::<loom_shared::navigate_outcome::WaitOutcome>(&bytes).map_err(
                    |e| {
                        LoomError::new(
                            LoomErrorCode::ShimFailure,
                            format!("shim {}: wait_for outcome decode: {e}", id.0),
                        )
                    },
                )
            }
            Ok(ShimResponse::Error { code, detail, .. }) => {
                self.record_failure(&id, shim_error_class(&code));
                Err(LoomError::new(
                    map_shim_code(code),
                    format!("shim {}: {}", id.0, detail),
                ))
            }
            Ok(other) => {
                self.record_failure(&id, FailureClass::Transport);
                Err(LoomError::new(
                    LoomErrorCode::ShimFailure,
                    format!("shim {}: unexpected non-Ok response: {other:?}", id.0),
                ))
            }
            Err(e) => {
                self.record_failure(&id, FailureClass::Transport);
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
        determinism_enabled: bool,
    ) -> Result<EvaluateOutcome, LoomError> {
        // action_id reserved for receipt correlation (Q5 plumbing).
        let _action_id = action_id;

        self.check_breaker(&id)?;

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
            // settle-capture (4b): per-session determinism toggle.
            determinism_enabled,
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
                self.record_failure(&id, shim_error_class(&code));
                Err(LoomError::new(
                    map_shim_code(code),
                    format!("shim {}: {}", id.0, detail),
                ))
            }
            Ok(other) => {
                self.record_failure(&id, FailureClass::Transport);
                Err(LoomError::new(
                    LoomErrorCode::ShimFailure,
                    format!("shim {}: unexpected non-Ok response: {other:?}", id.0),
                ))
            }
            Err(e) => {
                self.record_failure(&id, FailureClass::Transport);
                Err(e)
            }
        }
    }

    /// Resolve a CSS selector to a file input and set its files via CDP
    /// `DOM.setFileInputFiles`. Issues the sequence
    /// `DOM.getDocument` → `DOM.querySelector` → `DOM.setFileInputFiles`
    /// against the session's target. Paths are already validated +
    /// canonicalized daemon-side (upload_guard) before reaching here.
    ///
    /// Outcomes:
    ///   - `Ok(SetInputFilesOutcome::Ok { file_count })` on success.
    ///   - `Ok(SelectorNotFound)` when querySelector returns nodeId == 0.
    ///   - `Ok(NotAFileInput)` when setFileInputFiles errors on a resolved node.
    ///   - `Err(LoomError)` for transport / breaker / protocol failures.
    #[allow(clippy::too_many_arguments)]
    pub async fn send_set_input_files(
        &self,
        id: ShimId,
        action_id: String,
        session_id: u64,
        target_id: u64,
        selector: String,
        files: Vec<String>,
        budget_ms: u64,
        seed: Seed,
        epoch_ms: EpochMs,
        determinism_enabled: bool,
    ) -> Result<SetInputFilesOutcome, LoomError> {
        use ciborium::value::{Integer, Value};
        let _action_id = action_id;

        self.check_breaker(&id)?;

        let config = self.configs.get(&id).map(|c| c.clone()).ok_or_else(|| {
            LoomError::new(
                LoomErrorCode::ShimFailure,
                format!("shim {} not registered", id.0),
            )
        })?;

        let process = self.get_or_spawn(&id, &config).await?;
        let recv_ms = budget_ms.max(config.recv_timeout_ms);

        // Lazy-spawn the session target (idempotent), same as send_evaluate —
        // so a set_input_files before an explicit SpawnTarget still resolves
        // against a real target rather than the bootstrap context.
        let spawn_request = ShimRequest::SpawnTarget {
            request_id: 0,
            session_id,
            profile: "default".to_string(),
            seed,
            epoch_ms,
            // settle-capture (4b): per-session determinism toggle.
            determinism_enabled,
        };
        let _ = send_and_await(
            &process,
            spawn_request,
            Duration::from_millis(config.send_timeout_ms),
            Duration::from_millis(config.recv_timeout_ms),
        )
        .await;

        // One raw CdpSend round-trip → raw CDP result Value (or Err on a
        // shim-level error envelope). `step_err_is_app` lets the caller treat
        // a CDP error at a specific step as an application outcome.
        let cdp = |method: &'static str, params: Value| {
            let process = process.clone();
            let send_to = Duration::from_millis(config.send_timeout_ms);
            let recv_to = Duration::from_millis(recv_ms);
            async move {
                let request = ShimRequest::CdpSend {
                    request_id: 0,
                    session_id,
                    target_id,
                    message: CdpMessage {
                        method: method.into(),
                        params,
                    },
                };
                send_and_await(&process, request, send_to, recv_to).await
            }
        };

        // Step 1: DOM.getDocument(depth=0) → root nodeId.
        let root_resp = cdp(
            "DOM.getDocument",
            Value::Map(vec![(
                Value::Text("depth".into()),
                Value::Integer(Integer::from(0)),
            )]),
        )
        .await;
        let root_node_id = match root_resp {
            Ok(ShimResponse::Ok { payload, .. }) => cbor_get(&payload, "root")
                .and_then(|r| cbor_get(r, "nodeId"))
                .and_then(cbor_u64)
                .ok_or_else(|| {
                    LoomError::new(
                        LoomErrorCode::ShimFailure,
                        format!("shim {}: getDocument: no root.nodeId", id.0),
                    )
                })?,
            Ok(ShimResponse::Error { code, detail, .. }) => {
                self.record_failure(&id, shim_error_class(&code));
                return Err(LoomError::new(
                    map_shim_code(code),
                    format!("shim {}: {}", id.0, detail),
                ));
            }
            Ok(other) => {
                self.record_failure(&id, FailureClass::Transport);
                return Err(LoomError::new(
                    LoomErrorCode::ShimFailure,
                    format!("shim {}: getDocument unexpected: {other:?}", id.0),
                ));
            }
            Err(e) => {
                self.record_failure(&id, FailureClass::Transport);
                return Err(e);
            }
        };

        // Step 2: DOM.querySelector(root, selector) → nodeId (0 == not found).
        let qs_resp = cdp(
            "DOM.querySelector",
            Value::Map(vec![
                (
                    Value::Text("nodeId".into()),
                    Value::Integer(Integer::from(root_node_id)),
                ),
                (Value::Text("selector".into()), Value::Text(selector)),
            ]),
        )
        .await;
        let node_id = match qs_resp {
            Ok(ShimResponse::Ok { payload, .. }) => {
                cbor_get(&payload, "nodeId").and_then(cbor_u64).unwrap_or(0)
            }
            Ok(ShimResponse::Error { code, detail, .. }) => {
                self.record_failure(&id, shim_error_class(&code));
                return Err(LoomError::new(
                    map_shim_code(code),
                    format!("shim {}: {}", id.0, detail),
                ));
            }
            Ok(other) => {
                self.record_failure(&id, FailureClass::Transport);
                return Err(LoomError::new(
                    LoomErrorCode::ShimFailure,
                    format!("shim {}: querySelector unexpected: {other:?}", id.0),
                ));
            }
            Err(e) => {
                self.record_failure(&id, FailureClass::Transport);
                return Err(e);
            }
        };
        if node_id == 0 {
            // No match — typed application outcome (not a transport failure).
            self.record_success(&id);
            return Ok(SetInputFilesOutcome::SelectorNotFound);
        }

        // Step 3: DOM.setFileInputFiles(nodeId, files). A CDP error on a
        // RESOLVED node means it isn't a file input (or it rejected the files).
        let file_count = files.len() as u32;
        let files_val = Value::Array(files.into_iter().map(Value::Text).collect());
        let set_resp = cdp(
            "DOM.setFileInputFiles",
            Value::Map(vec![
                (
                    Value::Text("nodeId".into()),
                    Value::Integer(Integer::from(node_id)),
                ),
                (Value::Text("files".into()), files_val),
            ]),
        )
        .await;
        match set_resp {
            Ok(ShimResponse::Ok { .. }) => {
                self.record_success(&id);
                Ok(SetInputFilesOutcome::Ok { file_count })
            }
            Ok(ShimResponse::Error { .. }) => {
                // Node resolved but setFileInputFiles rejected it → not a file input.
                // This is an application outcome, not a shim breaker failure.
                self.record_success(&id);
                Ok(SetInputFilesOutcome::NotAFileInput)
            }
            Ok(other) => {
                self.record_failure(&id, FailureClass::Transport);
                Err(LoomError::new(
                    LoomErrorCode::ShimFailure,
                    format!("shim {}: setFileInputFiles unexpected: {other:?}", id.0),
                ))
            }
            Err(e) => {
                self.record_failure(&id, FailureClass::Transport);
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
            self.spawn_locks.remove(&key);
        }
        // Drop the ULID -> wire-id mapping for this session. `host_session_ids`
        // is keyed by the raw session ULID (not the `:<sid>`-suffixed ShimId),
        // so it is removed here independently of the process/state/config maps.
        // Without this the map grows monotonically under session churn — a
        // small but unbounded leak, with stale mappings persisting forever
        // (audit 2026-06-10).
        self.host_session_ids.remove(session_id);
        // Remove the per-session chromium profile dir
        // (`<tmp>/loom-chromium-<session_id>`). Without this it leaks forever
        // and accumulates across a long-running daemon (a contributor to the
        // gradual degradation) — and leaves the session's cookies/state on disk.
        // Done AFTER shutdown_process so chromium has released `--user-data-dir`;
        // NotFound is fine (idempotent / already gone via crash reap). Offloaded
        // to a blocking thread: a recursive `remove_dir_all` of a populated
        // chromium profile can do real disk I/O and must not stall a Tokio
        // worker.
        let sid = session_id.to_string();
        let _ = tokio::task::spawn_blocking(move || remove_session_profile_dir(&sid)).await;
        // Reap any completed breaker-eviction cleanup tasks. Lock release
        // happens at scope exit; JoinSet::try_join_next is non-blocking.
        let mut set = self.cleanup_tasks.lock();
        while set.try_join_next().is_some() {}
    }

    /// Gate a send path on the circuit breaker.
    ///
    /// `Closed` (or untracked) passes. `Open` fail-fasts with
    /// `ShimBreakerOpen` until the open window (`breaker_open_ms`,
    /// default 5 s) has elapsed since `opened_at_ms`; the first call
    /// after expiry transitions the breaker to `HalfOpen` and proceeds
    /// as a recovery probe. `HalfOpen` admits calls as probes (no
    /// single-probe gating — an early-return path that skipped both
    /// `record_*` calls would otherwise wedge the breaker half-open
    /// forever): the first probe success closes the breaker
    /// (`record_success`), the first probe failure re-opens it with a
    /// fresh window (`record_failure`).
    pub(crate) fn check_breaker(&self, id: &ShimId) -> Result<(), LoomError> {
        let Some(mut state) = self.states.get_mut(id) else {
            return Ok(());
        };
        match state.breaker {
            BreakerState::Closed | BreakerState::HalfOpen => Ok(()),
            BreakerState::Open => {
                let open_ms = self
                    .configs
                    .get(id)
                    .map(|c| c.breaker_open_ms)
                    .unwrap_or(5_000);
                let expired = state
                    .opened_at_ms
                    .is_none_or(|t| now_ms().saturating_sub(t) >= open_ms);
                if expired {
                    state.breaker = BreakerState::HalfOpen;
                    Ok(())
                } else {
                    Err(LoomError::new(
                        LoomErrorCode::ShimBreakerOpen,
                        format!("shim {} circuit breaker is open", id.0),
                    ))
                }
            }
        }
    }

    /// Increment the breaker counter on a failure. Opens the breaker at
    /// threshold (or immediately on a failed `HalfOpen` probe, with a
    /// fresh window either way). `Transport` failures additionally evict
    /// the live process so the next admitted call triggers a fresh
    /// spawn; `Application` failures keep the healthy subprocess (and
    /// its mid-session browser state) alive — see `FailureClass`.
    pub(crate) fn record_failure(&self, id: &ShimId, class: FailureClass) {
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
            restart_count: 0,
            last_restart_at_ms: None,
        });
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        if state.breaker == BreakerState::HalfOpen || state.consecutive_failures >= threshold {
            state.breaker = BreakerState::Open;
            state.opened_at_ms = Some(now_ms());
        }
        drop(state);
        // Evict the live process on transport failures only — the
        // subprocess/socket is unhealthy and the next admitted call must
        // respawn. Application failures came from a live, responsive
        // shim; evicting would kill a healthy Chromium and destroy its
        // mid-session browser state.
        if class == FailureClass::Transport {
            if let Some((_, p)) = self.processes.remove(id) {
                let mut set = self.cleanup_tasks.lock();
                set.spawn(shutdown_process(p));
                // Opportunistically reap completed cleanups so the JoinSet
                // doesn't grow unbounded across many breaker evictions.
                while set.try_join_next().is_some() {}
            }
        }
    }

    /// Reset the breaker to `Closed`. Also the `HalfOpen` → `Closed`
    /// transition: a successful recovery probe lands here.
    pub(crate) fn record_success(&self, id: &ShimId) {
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
                (
                    s.id.clone(),
                    s.breaker,
                    s.consecutive_failures,
                    s.opened_at_ms,
                )
            })
            .collect()
    }

    pub fn breaker_state(&self, id: &ShimId) -> Option<BreakerState> {
        self.states.get(id).map(|s| s.breaker)
    }

    /// Clone the full `ShimState` for `id`, if tracked. Used by
    /// `daemon.health({deep:true})` to surface restart bookkeeping per shim.
    pub fn shim_state(&self, id: &ShimId) -> Option<ShimStateSnapshot> {
        self.states.get(id).map(|s| ShimStateSnapshot {
            id: s.id.clone(),
            breaker: s.breaker,
            consecutive_failures: s.consecutive_failures,
            opened_at_ms: s.opened_at_ms,
            restart_count: s.restart_count,
            last_restart_at_ms: s.last_restart_at_ms,
        })
    }

    /// Snapshot every tracked shim id. Used by `daemon.health({deep:true})`
    /// to iterate the probe fan-out. Iterates `processes` (live subprocesses)
    /// rather than `states` (which can have stale entries for evicted
    /// shims) so probes only target running children.
    pub fn list_shim_ids(&self) -> Vec<ShimId> {
        self.processes.iter().map(|kv| kv.key().clone()).collect()
    }

    /// Probe a live shim for its self-reported `ShimHealthInfo`. Used by
    /// `daemon.health({deep:true})`. Does NOT lazy-spawn — if the shim is
    /// not running, returns `LoomErrorCode::ShimFailure`.
    ///
    /// Timeout: `LOOM_PROBE_TIMEOUT_MS` env override (default 1000). The
    /// timeout is enforced INSIDE `send_and_await`, so cancelling the
    /// returned future cannot leak entries from `process.pending`.
    pub async fn probe_health(&self, id: &ShimId) -> Result<ShimHealthInfo, LoomError> {
        let process = self.processes.get(id).map(|p| p.clone()).ok_or_else(|| {
            LoomError::new(
                LoomErrorCode::ShimFailure,
                format!("shim {} is not running — cannot probe", id.0),
            )
        })?;
        let recv_ms = std::env::var("LOOM_PROBE_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(1_000);
        // request_id is overwritten by send_and_await via set_request_id.
        let req = ShimRequest::Health { request_id: 0 };
        match send_and_await(
            &process,
            req,
            Duration::from_millis(500),
            Duration::from_millis(recv_ms),
        )
        .await?
        {
            ShimResponse::Ok { payload, .. } => {
                // Re-encode the ciborium Value and decode as ShimHealthInfo.
                let bytes = ciborium_to_vec(&payload).map_err(|e| {
                    LoomError::new(
                        LoomErrorCode::ShimFailure,
                        format!("re-encode ShimHealthInfo payload: {e}"),
                    )
                })?;
                ciborium_from_slice::<ShimHealthInfo>(&bytes).map_err(|e| {
                    LoomError::new(
                        LoomErrorCode::ShimFailure,
                        format!("decode ShimHealthInfo: {e}"),
                    )
                })
            }
            ShimResponse::Error { code, detail, .. } => Err(LoomError::new(
                LoomErrorCode::ShimFailure,
                format!("shim health probe returned error {code:?}: {detail}"),
            )),
            other => Err(LoomError::new(
                LoomErrorCode::ShimFailure,
                format!("shim health probe: unexpected response: {other:?}"),
            )),
        }
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

/// Validate a session id against the canonical safe charset before it is used
/// to build a filesystem path or a process-match pattern. Guards against path
/// traversal (`../`) in the profile-dir `remove_dir_all` and against injection
/// into the watcher's `pkill -f user-data-dir=...` pattern. Session ids are
/// daemon-generated, but this is defense-in-depth: a malformed id is refused,
/// never acted on.
fn is_safe_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.len() <= 64
        && session_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The per-session chromium profile dir, mirroring the path the host exports as
/// `LOOM_SHIM_USER_DATA_DIR` (`<tmp>/loom-chromium-<session_id>`). `None` if the
/// id fails validation (caller skips cleanup rather than touching an unsafe path).
fn session_profile_dir(session_id: &str) -> Option<PathBuf> {
    if !is_safe_session_id(session_id) {
        return None;
    }
    Some(std::env::temp_dir().join(format!("loom-chromium-{session_id}")))
}

/// Remove the per-session chromium profile dir on session close. Idempotent:
/// a missing dir (already reaped, or never created for a session that never
/// navigated) is success; other errors are logged, not propagated.
fn remove_session_profile_dir(session_id: &str) {
    let Some(dir) = session_profile_dir(session_id) else {
        tracing::warn!(
            session = %session_id,
            "refusing to clean profile dir for unsafe session id"
        );
        return;
    };
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => tracing::debug!(dir = %dir.display(), "removed session profile dir"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(dir = %dir.display(), error = %e, "profile dir cleanup failed"),
    }
}

fn map_shim_code(code: loom_shared::shim_protocol::ShimErrorCode) -> LoomErrorCode {
    use loom_shared::shim_protocol::ShimErrorCode as E;
    match code {
        E::ChromiumUnavailable => LoomErrorCode::ShimFailure,
        E::CdpTimeout => LoomErrorCode::ShimTimeout,
        E::CdpProtocolError | E::TargetUnknown | E::ShimInternalError => LoomErrorCode::ShimFailure,
    }
}

/// Classify a shim-REPORTED error envelope for `record_failure`.
/// `ChromiumUnavailable` means the shim's browser is gone and its own
/// restart budget is exhausted — the supervisor contract hands the
/// respawn decision to the ShimManager, so it is transport class (evict;
/// the next admitted call respawns the whole shim). Everything else came
/// from a live shim with a running Chromium (bad CDP params, unknown
/// target, slow page, internal shim error) — application class: count
/// toward the breaker, keep the browser alive.
fn shim_error_class(code: &loom_shared::shim_protocol::ShimErrorCode) -> FailureClass {
    use loom_shared::shim_protocol::ShimErrorCode as E;
    match code {
        E::ChromiumUnavailable => FailureClass::Transport,
        E::CdpTimeout | E::CdpProtocolError | E::TargetUnknown | E::ShimInternalError => {
            FailureClass::Application
        }
    }
}

// ─── Evaluate types ─────────────────────────────────────────────────────────

/// Outcome of a `set_input_files` CDP sequence. `SelectorNotFound` /
/// `NotAFileInput` are application outcomes (the host maps them to typed
/// wire `kind` strings), distinct from transport `Err(LoomError)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetInputFilesOutcome {
    Ok { file_count: u32 },
    SelectorNotFound,
    NotAFileInput,
}

/// Fetch a string-keyed field from a CBOR map `Value`.
fn cbor_get<'a>(v: &'a ciborium::value::Value, key: &str) -> Option<&'a ciborium::value::Value> {
    if let ciborium::value::Value::Map(entries) = v {
        for (k, val) in entries {
            if let ciborium::value::Value::Text(t) = k {
                if t == key {
                    return Some(val);
                }
            }
        }
    }
    None
}

/// Interpret a CBOR `Value` as a non-negative `u64` (CDP nodeIds).
fn cbor_u64(v: &ciborium::value::Value) -> Option<u64> {
    if let ciborium::value::Value::Integer(i) = v {
        u64::try_from(i128::from(*i)).ok()
    } else {
        None
    }
}

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
