// Dispatcher — `tokio::select!` non-blocking router.
//
// # Contract semantics
// - **Non-blocking dispatch.** Multiple in-flight
//   `cdp_send` requests do NOT serialise. Each request spawns its own
//   tokio task; chromiumoxide multiplexes responses by CDP message id.
//   `cdp_send` p99 ≤ 50ms is achievable only because of this.
// - **Routing by `kind`.** `SpawnTarget` / `PageNavigate` / `PageClose`
//   route to `TargetManager`; `CdpSend` routes to `ActionExecutor`;
//   `Shutdown` triggers cooperative drain via `Supervisor::shutdown`.
// - **Crash invalidation hook.** `Supervisor` calls
//   `invalidate_in_flight()` on Chromium crash; all in-flight requests
//   resolve to `ShimErrorCode::ChromiumUnavailable` immediately
//   (state-invalidation cascade, §3.3).
// - **Unknown kind handling.** Returns `ShimErrorCode::ShimInternalError`
//   with `detail: "unknown kind: <name>"`. Does NOT crash the shim.
// - **No CDP payload escape.** Dispatcher passes typed
//   `CdpMessage` values to lower layers; never inspects or transforms
//   CDP method strings beyond enum routing.

use crate::action_executor::action_executor::{ActionExecutor, ActionResult};
use crate::ipc_endpoint::ipc_endpoint::{
    ResponseSender, SessionId, ShimErrorCode, ShimHealthInfo, ShimRequest, ShimResponse, TargetId,
};
use crate::target_manager::target_manager::TargetManager;
use async_trait::async_trait;
use ciborium::value::Value as CborValue;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot};

/// In-flight request bookkeeping. Keyed by `(session_id, message_id)`
/// where `message_id` is the chromiumoxide CDP id when applicable, or
/// a synthesized one for non-CDP requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InFlightKey {
    pub session_id: SessionId,
    pub target_id: Option<TargetId>,
    pub local_id: u64,
}

/// Concrete dispatcher.
pub struct ShimDispatcher {
    pub(crate) target_manager: Arc<dyn TargetManager>,
    pub(crate) action_executor: Arc<dyn ActionExecutor>,
    pub(crate) response_tx: ResponseSender,
    pub(crate) invalidated: Arc<AtomicBool>,
    /// Subprocess startup instant — basis for `ShimHealthInfo.uptime_ms`.
    pub(crate) started_at: Instant,
    /// Counter of non-meta requests served. Incremented after the
    /// Shutdown/Health intercept in `run`. Used in `ShimHealthInfo`.
    pub(crate) requests_served: Arc<AtomicU64>,
    /// Epoch ms of the most recent non-meta request (0 = none yet).
    pub(crate) last_request_at_ms: Arc<AtomicU64>,
}

impl ShimDispatcher {
    pub fn new(
        target_manager: Arc<dyn TargetManager>,
        action_executor: Arc<dyn ActionExecutor>,
        response_tx: ResponseSender,
    ) -> Self {
        Self {
            target_manager,
            action_executor,
            response_tx,
            invalidated: Arc::new(AtomicBool::new(false)),
            started_at: Instant::now(),
            requests_served: Arc::new(AtomicU64::new(0)),
            last_request_at_ms: Arc::new(AtomicU64::new(0)),
        }
    }
}

/// Public dispatcher trait.
#[async_trait]
pub trait Dispatcher: Send + Sync {
    /// Drives the request loop. Reads `ShimRequest` values and routes
    /// them by `kind` onto the shim's tokio runtime via
    /// `tokio::select!`. Returns when `Shutdown` arrives or when the
    /// upstream channel closes.
    async fn run(
        &self,
        request_rx: mpsc::Receiver<ShimRequest>,
        shutdown_rx: oneshot::Receiver<()>,
    ) -> Result<(), DispatchError>;

    /// State-invalidation hook called by `Supervisor` on Chromium
    /// crash. Resolves all in-flight requests with
    /// `ShimErrorCode::ChromiumUnavailable`.
    fn invalidate_in_flight(&self, reason: &str);
}

#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("upstream request channel closed")]
    ChannelClosed,
    #[error("response channel closed (loom-host disappeared)")]
    ResponseChannelClosed,
}

#[async_trait]
impl Dispatcher for ShimDispatcher {
    async fn run(
        &self,
        mut request_rx: mpsc::Receiver<ShimRequest>,
        mut shutdown_rx: oneshot::Receiver<()>,
    ) -> Result<(), DispatchError> {
        loop {
            tokio::select! {
                req = request_rx.recv() => {
                    let Some(req) = req else { return Ok(()); };
                    if matches!(req, ShimRequest::Shutdown { .. }) {
                        // Drain pending and exit.
                        // Echo a final Ok for the Shutdown request so the
                        // host sees a clean ack before the socket closes.
                        if let ShimRequest::Shutdown { request_id } = req {
                            let _ = self.response_tx
                                .send(make_ok_response(request_id, None, CborValue::Null))
                                .await;
                        }
                        return Ok(());
                    }
                    if let ShimRequest::Health { request_id } = req {
                        // Process-local probe — respond inline (no spawn,
                        // no executor traffic). Excludes itself from the
                        // requests_served counter so deep-health doesn't
                        // inflate the user-work metric.
                        let info = ShimHealthInfo {
                            uptime_ms: self.started_at.elapsed().as_millis() as u64,
                            requests_served: self.requests_served.load(Ordering::Relaxed),
                            last_request_at_ms: {
                                let v = self.last_request_at_ms.load(Ordering::Relaxed);
                                if v == 0 { None } else { Some(v) }
                            },
                        };
                        let payload = info_to_cbor(&info);
                        let _ = self.response_tx
                            .send(make_ok_response(request_id, None, payload))
                            .await;
                        continue;
                    }
                    // Non-meta request — bump observability counters before
                    // dispatch (see ShimHealthInfo semantics).
                    self.requests_served.fetch_add(1, Ordering::Relaxed);
                    self.last_request_at_ms.store(epoch_ms_now(), Ordering::Relaxed);
                    let response_tx = self.response_tx.clone();
                    let target_manager = self.target_manager.clone();
                    let action_executor = self.action_executor.clone();
                    let invalidated = self.invalidated.clone();
                    tokio::spawn(async move {
                        let resp = handle_request(
                            req,
                            target_manager,
                            action_executor,
                            invalidated,
                        ).await;
                        let _ = response_tx.send(resp).await;
                    });
                }
                _ = &mut shutdown_rx => {
                    return Ok(());
                }
            }
        }
    }

    fn invalidate_in_flight(&self, reason: &str) {
        // Mark the dispatcher invalidated so any in-flight handle_request
        // tasks that haven't yet hit the executor short-circuit to
        // ChromiumUnavailable. Already-dispatched calls into the executor
        // surface the error via CdpError::ConnectionClosed.
        tracing::warn!(reason, "dispatcher invalidated by supervisor");
        self.invalidated.store(true, Ordering::SeqCst);
    }
}

/// Process a single non-Shutdown request and produce a ShimResponse with
/// the matching request_id echoed. The response is always shaped — errors
/// never panic; they become `ShimResponse::Error`.
async fn handle_request(
    req: ShimRequest,
    target_manager: Arc<dyn TargetManager>,
    action_executor: Arc<dyn ActionExecutor>,
    invalidated: Arc<AtomicBool>,
) -> ShimResponse {
    let (request_id, session_id) = request_correlation(&req);

    if invalidated.load(Ordering::SeqCst) {
        return make_error_response(
            request_id,
            session_id,
            ShimErrorCode::ChromiumUnavailable,
            "dispatcher invalidated — Chromium subprocess crashed",
        );
    }

    match req {
        ShimRequest::SpawnTarget {
            request_id,
            session_id,
            profile,
            seed,
            epoch_ms,
            determinism_enabled,
            audio_enabled,
        } => {
            match target_manager
                .create_new_target(
                    session_id,
                    profile,
                    seed,
                    epoch_ms,
                    determinism_enabled,
                    audio_enabled,
                )
                .await
            {
                Ok(target_id) => {
                    // ciborium::Integer::from(u64) is fallible for u64::MAX; use the
                    // i128 round-trip via try_from with a fallback to Null when out
                    // of range (target_id is u64 so the upper half is in range).
                    // try_from is intentional: u64 > i128::MAX maps to Null (target_id is u64
                    // so in practice always fits, but the fallback preserves correctness).
                    #[allow(clippy::unnecessary_fallible_conversions)]
                    let payload = ciborium::value::Integer::try_from(target_id)
                        .map(CborValue::Integer)
                        .unwrap_or(CborValue::Null);
                    make_ok_response(request_id, Some(session_id), payload)
                }
                Err(e) => {
                    let detail = e.to_string();
                    make_error_response(
                        request_id,
                        Some(session_id),
                        ShimErrorCode::from(e),
                        detail,
                    )
                }
            }
        }
        ShimRequest::CdpSend {
            request_id,
            session_id,
            target_id,
            message,
        } => match action_executor.cdp_send(target_id, message, None).await {
            Ok(ActionResult::CdpResult { result }) => {
                make_ok_response(request_id, Some(session_id), result)
            }
            Ok(other) => {
                make_ok_response(request_id, Some(session_id), action_result_to_cbor(other))
            }
            Err(mut shim_resp) => {
                overwrite_correlation(&mut shim_resp, request_id, Some(session_id));
                shim_resp
            }
        },
        ShimRequest::PageNavigate {
            request_id,
            session_id,
            target_id,
            url,
            seed,
            epoch_ms,
            blocklist_enabled,
            until,
            determinism_enabled,
            audio_enabled,
        } => {
            // Lazy-spawn: when the host doesn't know the target yet (target_id == 0),
            // create one here so the per-session seed reaches the inject path even
            // when the host calls navigate without a prior explicit SpawnTarget.
            // Idempotent at the target_manager level.
            let effective_target_id = if target_id == 0 {
                match target_manager
                    .create_new_target(
                        session_id,
                        "default".into(),
                        seed,
                        epoch_ms,
                        determinism_enabled,
                        audio_enabled,
                    )
                    .await
                {
                    Ok(t) => t,
                    Err(e) => {
                        let detail = e.to_string();
                        return make_error_response(
                            request_id,
                            Some(session_id),
                            ShimErrorCode::from(e),
                            detail,
                        );
                    }
                }
            } else {
                target_id
            };
            let settle_mode =
                crate::readiness_monitor::SettleMode::parse(&until).unwrap_or_default();
            match action_executor
                .page_navigate(
                    effective_target_id,
                    url,
                    None,
                    blocklist_enabled,
                    settle_mode,
                    determinism_enabled,
                    audio_enabled,
                )
                .await
            {
                Ok(result) => {
                    make_ok_response(request_id, Some(session_id), action_result_to_cbor(result))
                }
                Err(mut shim_resp) => {
                    overwrite_correlation(&mut shim_resp, request_id, Some(session_id));
                    shim_resp
                }
            }
        }
        ShimRequest::WaitFor {
            request_id,
            session_id,
            target_id,
            until,
        } => {
            // settle-capture: standalone readiness wait on the current page.
            // The host issues an idempotent SpawnTarget before this (mirroring
            // the evaluate path), so target_id resolves to the session's
            // determinism-injected target. No navigation, no capture.
            let settle_mode =
                crate::readiness_monitor::SettleMode::parse(&until).unwrap_or_default();
            match action_executor.wait_for(target_id, settle_mode, None).await {
                Ok(result) => {
                    make_ok_response(request_id, Some(session_id), action_result_to_cbor(result))
                }
                Err(mut shim_resp) => {
                    overwrite_correlation(&mut shim_resp, request_id, Some(session_id));
                    shim_resp
                }
            }
        }
        ShimRequest::PageClose {
            request_id,
            session_id,
            target_id,
        } => match action_executor.page_close(target_id).await {
            Ok(result) => {
                make_ok_response(request_id, Some(session_id), action_result_to_cbor(result))
            }
            Err(mut shim_resp) => {
                overwrite_correlation(&mut shim_resp, request_id, Some(session_id));
                shim_resp
            }
        },
        ShimRequest::GetNetworkLog {
            request_id,
            session_id,
            target_id,
        } => {
            // Observation-only: read the accumulator the Network.* handler has
            // been filling since the last navigate. No CDP round-trip.
            let (network_entries, network_entries_truncated) =
                action_executor.read_network_log(target_id);
            let outcome = loom_shared::navigate_outcome::NetworkLogOutcome {
                network_entries,
                network_entries_truncated,
            };
            make_ok_response(
                request_id,
                Some(session_id),
                network_log_outcome_to_cbor(&outcome),
            )
        }
        ShimRequest::StartRecording {
            request_id,
            session_id,
            target_id,
            max_duration_ms,
            max_bytes,
            frame_rate,
        } => {
            // `sanitized` clamps a 0/absent cap to the safe DEFAULT so the public
            // API can't disable the safety caps (ship-council CRIT).
            let caps = crate::screencast_recorder::Caps::sanitized(
                max_duration_ms,
                max_bytes,
                frame_rate as u64,
            );
            match action_executor.start_recording(target_id, caps).await {
                Ok(()) => make_ok_response(request_id, Some(session_id), CborValue::Null),
                Err(detail) => make_error_response(
                    request_id,
                    Some(session_id),
                    ShimErrorCode::CdpProtocolError,
                    detail,
                ),
            }
        }
        ShimRequest::StopRecording {
            request_id,
            session_id,
            target_id,
        } => {
            // Always returns an outcome (errors embedded). The host inspects
            // `error`/`stop_reason` and emits an error receipt if needed.
            let outcome = action_executor.stop_recording(target_id).await;
            make_ok_response(
                request_id,
                Some(session_id),
                screencast_outcome_to_cbor(&outcome),
            )
        }
        ShimRequest::InjectAudio {
            request_id,
            session_id,
            target_id,
            audio_bytes,
            await_playout,
        } => {
            // Daemon already resolved + size-bounded the payload. The typed error
            // KIND (no_microphone_request / audio_decode_failed / inject_timeout /
            // audio_not_enabled / …) rides the `detail` string; the daemon derives
            // the receipt kind from it.
            match action_executor
                .inject_audio(session_id, target_id, audio_bytes, await_playout)
                .await
            {
                Ok(outcome) => make_ok_response(
                    request_id,
                    Some(session_id),
                    audio_inject_outcome_to_cbor(&outcome),
                ),
                Err(detail) => make_error_response(
                    request_id,
                    Some(session_id),
                    ShimErrorCode::CdpProtocolError,
                    detail,
                ),
            }
        }
        ShimRequest::StartAudioCapture {
            request_id,
            session_id,
            target_id,
            max_duration_ms,
            max_bytes,
        } => {
            // `sanitized` clamps a 0/absent cap to the safe DEFAULT so the public
            // API can't disable the safety caps (parity with StartRecording).
            let caps = crate::audio_bridge::Caps::sanitized(max_duration_ms, max_bytes);
            match action_executor
                .start_audio_capture(session_id, target_id, caps)
                .await
            {
                Ok(()) => make_ok_response(request_id, Some(session_id), CborValue::Null),
                Err(detail) => make_error_response(
                    request_id,
                    Some(session_id),
                    ShimErrorCode::CdpProtocolError,
                    detail,
                ),
            }
        }
        ShimRequest::StopAudioCapture {
            request_id,
            session_id,
            target_id,
        } => {
            // Always returns an outcome (errors embedded). The host inspects
            // `error`/`stop_reason` and emits an error receipt if needed.
            let outcome = action_executor
                .stop_audio_capture(session_id, target_id)
                .await;
            make_ok_response(
                request_id,
                Some(session_id),
                audio_capture_outcome_to_cbor(&outcome),
            )
        }
        ShimRequest::Shutdown { .. } => unreachable!("Shutdown handled in run loop"),
        ShimRequest::Health { .. } => unreachable!("Health handled in run loop"),
    }
}

/// Serialise a `ScreencastOutcome` to CBOR for `ShimResponse::Ok.payload`
/// (mirrors `network_log_outcome_to_cbor`).
fn screencast_outcome_to_cbor(
    outcome: &loom_shared::navigate_outcome::ScreencastOutcome,
) -> CborValue {
    let mut bytes = Vec::new();
    if ciborium::ser::into_writer(outcome, &mut bytes).is_err() {
        return CborValue::Null;
    }
    ciborium::de::from_reader(&bytes[..]).unwrap_or(CborValue::Null)
}

/// Serialise an `AudioInjectOutcome` to CBOR for `ShimResponse::Ok.payload`
/// (mirrors `screencast_outcome_to_cbor`).
fn audio_inject_outcome_to_cbor(
    outcome: &loom_shared::navigate_outcome::AudioInjectOutcome,
) -> CborValue {
    let mut bytes = Vec::new();
    if ciborium::ser::into_writer(outcome, &mut bytes).is_err() {
        return CborValue::Null;
    }
    ciborium::de::from_reader(&bytes[..]).unwrap_or(CborValue::Null)
}

/// Serialise an `AudioCaptureOutcome` to CBOR for `ShimResponse::Ok.payload`
/// (mirrors `screencast_outcome_to_cbor`).
fn audio_capture_outcome_to_cbor(
    outcome: &loom_shared::navigate_outcome::AudioCaptureOutcome,
) -> CborValue {
    let mut bytes = Vec::new();
    if ciborium::ser::into_writer(outcome, &mut bytes).is_err() {
        return CborValue::Null;
    }
    ciborium::de::from_reader(&bytes[..]).unwrap_or(CborValue::Null)
}

/// Serialise a `NetworkLogOutcome` to a structured `CborValue` for the
/// `ShimResponse::Ok.payload` (mirrors `action_result_to_cbor`).
fn network_log_outcome_to_cbor(
    outcome: &loom_shared::navigate_outcome::NetworkLogOutcome,
) -> CborValue {
    let mut bytes = Vec::new();
    if ciborium::ser::into_writer(outcome, &mut bytes).is_err() {
        return CborValue::Null;
    }
    ciborium::de::from_reader(&bytes[..]).unwrap_or(CborValue::Null)
}

fn request_correlation(req: &ShimRequest) -> (u64, Option<SessionId>) {
    match req {
        ShimRequest::SpawnTarget {
            request_id,
            session_id,
            ..
        }
        | ShimRequest::CdpSend {
            request_id,
            session_id,
            ..
        }
        | ShimRequest::PageNavigate {
            request_id,
            session_id,
            ..
        }
        | ShimRequest::PageClose {
            request_id,
            session_id,
            ..
        }
        | ShimRequest::GetNetworkLog {
            request_id,
            session_id,
            ..
        }
        | ShimRequest::WaitFor {
            request_id,
            session_id,
            ..
        }
        | ShimRequest::StartRecording {
            request_id,
            session_id,
            ..
        }
        | ShimRequest::StopRecording {
            request_id,
            session_id,
            ..
        }
        | ShimRequest::InjectAudio {
            request_id,
            session_id,
            ..
        }
        | ShimRequest::StartAudioCapture {
            request_id,
            session_id,
            ..
        }
        | ShimRequest::StopAudioCapture {
            request_id,
            session_id,
            ..
        } => (*request_id, Some(*session_id)),
        ShimRequest::Shutdown { request_id } => (*request_id, None),
        ShimRequest::Health { request_id } => (*request_id, None),
    }
}

fn epoch_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Serialize `ShimHealthInfo` into a CBOR `Value` for use as a
/// `ShimResponse::Ok.payload`.
fn info_to_cbor(info: &ShimHealthInfo) -> CborValue {
    let mut bytes = Vec::new();
    if ciborium::ser::into_writer(info, &mut bytes).is_err() {
        return CborValue::Null;
    }
    ciborium::de::from_reader(&bytes[..]).unwrap_or(CborValue::Null)
}

fn overwrite_correlation(
    resp: &mut ShimResponse,
    new_request_id: u64,
    new_session_id: Option<SessionId>,
) {
    match resp {
        ShimResponse::Ok {
            request_id,
            session_id,
            ..
        }
        | ShimResponse::Error {
            request_id,
            session_id,
            ..
        } => {
            *request_id = new_request_id;
            *session_id = new_session_id;
        }
        // CdpEvent / LogLine are not request-correlated; leave alone.
        _ => {}
    }
}

fn action_result_to_cbor(result: ActionResult) -> CborValue {
    // Serialise via ciborium into Vec<u8>, then re-parse as CborValue so
    // the response payload preserves the structured shape.
    let mut bytes = Vec::new();
    if ciborium::ser::into_writer(&result, &mut bytes).is_err() {
        return CborValue::Null;
    }
    ciborium::de::from_reader(&bytes[..]).unwrap_or(CborValue::Null)
}

/// Pure helper: synthesise a `ShimResponse::Error` with the given
/// request id + session id + code + detail. Used by `Dispatcher` and by
/// `Supervisor::invalidate_in_flight`. `request_id` echoes the originating
/// request so the host can demultiplex.
pub fn make_error_response(
    request_id: u64,
    session_id: Option<SessionId>,
    code: ShimErrorCode,
    detail: impl Into<String>,
) -> ShimResponse {
    ShimResponse::Error {
        request_id,
        session_id,
        code,
        detail: detail.into(),
    }
}

/// Pure helper: synthesise a `ShimResponse::Ok` with a CBOR payload.
pub fn make_ok_response(
    request_id: u64,
    session_id: Option<SessionId>,
    payload: CborValue,
) -> ShimResponse {
    ShimResponse::Ok {
        request_id,
        session_id,
        payload,
    }
}

/// Categorise a `ShimRequest` by routing target. Pure function — used by
/// the dispatcher's main loop and by tests asserting routing tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteTarget {
    /// `SpawnTarget`, `PageNavigate`, `PageClose` → TargetManager.
    TargetManager,
    /// `CdpSend` → ActionExecutor.
    ActionExecutor,
    /// `Shutdown` → cooperative drain.
    Shutdown,
    /// `Health` → process-local liveness/uptime probe; never reaches an
    /// executor.
    Health,
}

pub fn route_target(req: &ShimRequest) -> RouteTarget {
    match req {
        ShimRequest::SpawnTarget { .. }
        | ShimRequest::PageNavigate { .. }
        | ShimRequest::PageClose { .. } => RouteTarget::TargetManager,
        // GetNetworkLog reads the accumulator; WaitFor runs the SettleDriver on
        // an already-existing target (the host issues an idempotent SpawnTarget
        // first) — both are executor work like CdpSend, not TargetManager
        // lifecycle ops.
        ShimRequest::CdpSend { .. }
        | ShimRequest::GetNetworkLog { .. }
        | ShimRequest::WaitFor { .. }
        | ShimRequest::StartRecording { .. }
        | ShimRequest::StopRecording { .. }
        | ShimRequest::InjectAudio { .. }
        | ShimRequest::StartAudioCapture { .. }
        | ShimRequest::StopAudioCapture { .. } => RouteTarget::ActionExecutor,
        ShimRequest::Shutdown { .. } => RouteTarget::Shutdown,
        ShimRequest::Health { .. } => RouteTarget::Health,
    }
}
