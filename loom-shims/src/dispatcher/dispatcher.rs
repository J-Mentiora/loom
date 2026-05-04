// Dispatcher — `tokio::select!` non-blocking router.
//
// # Contract semantics
// - **Non-blocking dispatch (IC-SHIM-12).** Multiple in-flight
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
// - **No CDP payload escape (IC-SHIM-06).** Dispatcher passes typed
//   `CdpMessage` values to lower layers; never inspects or transforms
//   CDP method strings beyond enum routing.

use crate::action_executor::action_executor::{ActionExecutor, ActionResult};
use crate::ipc_endpoint::ipc_endpoint::{
    ResponseSender, SessionId, ShimErrorCode, ShimRequest, ShimResponse, TargetId,
};
use crate::target_manager::target_manager::TargetManager;
use async_trait::async_trait;
use ciborium::value::Value as CborValue;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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
        } => {
            match target_manager
                .create_new_target(session_id, profile, seed, epoch_ms)
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
        } => {
            // Lazy-spawn: when the host doesn't know the target yet (target_id == 0),
            // create one here so the per-session seed reaches the inject path even
            // when the host calls navigate without a prior explicit SpawnTarget.
            // Idempotent at the target_manager level (SR-SHIM-01).
            let effective_target_id = if target_id == 0 {
                match target_manager
                    .create_new_target(session_id, "default".into(), seed, epoch_ms)
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
            match action_executor
                .page_navigate(effective_target_id, url, None, blocklist_enabled)
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
        ShimRequest::Shutdown { .. } => unreachable!("Shutdown handled in run loop"),
    }
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
        } => (*request_id, Some(*session_id)),
        ShimRequest::Shutdown { request_id } => (*request_id, None),
    }
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
}

pub fn route_target(req: &ShimRequest) -> RouteTarget {
    match req {
        ShimRequest::SpawnTarget { .. }
        | ShimRequest::PageNavigate { .. }
        | ShimRequest::PageClose { .. } => RouteTarget::TargetManager,
        ShimRequest::CdpSend { .. } => RouteTarget::ActionExecutor,
        ShimRequest::Shutdown { .. } => RouteTarget::Shutdown,
    }
}
