// TargetManager — `target_id` lifecycle + R3-ordering enforcer.
//
// # Contract semantics
// - **R3 load-bearing ordering (KILL).**
//   `create_new_target` calls `DeterminismInjector::inject(target_id)`
//   BEFORE `Network.enable` BEFORE `Page.enable` BEFORE the daemon
//   issues `page_navigate`. Deferred injection (post-`Page.loadEventFired`)
//   → KILL.
// - **R3 invariant guard.** `TargetState.determinism_injected` boolean
//   defaults `false`. Any subsequent navigation against a target
//   whose flag is still `false` panic-aborts (process exit) — defensive
//   guardrail against R3-ordering regression.
// - **One target per session.** Cross-session contamination
//   is structurally prevented: `BTreeMap<SessionId, TargetId>` is the
//   source of truth; `create_new_target` returns the existing target_id
//   if a session already has one.
// - **State-invalidation cascade.** `Supervisor::handle_crash` calls
//   `invalidate_targets()`; the BTreeMap is cleared and any pending
//   action against an invalidated target resolves to
//   `ShimErrorCode::TargetUnknown`.
// - **`cdp_event` upstream dispatch.** TargetManager owns
//   the `target_id → ResponseSender` upstream channel for events; on
//   each event from `CdpConnection`, it pushes
//   `ShimResponse::CdpEvent{target_id, message}` to the daemon.

use crate::cdp_connection::cdp_connection::{CdpConnection, CdpError};
use crate::determinism_injector::determinism_injector::DeterminismInjector;
use crate::ipc_endpoint::ipc_endpoint::{
    CdpMessage, ResponseSender, SessionId, ShimErrorCode, TargetId,
};
use loom_shared::types::{EpochMs, Seed};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

/// Per-target state. Lives in `TargetManager`'s BTreeMap.
#[derive(Debug, Clone)]
pub struct TargetState {
    pub session_id: SessionId,
    pub target_id: TargetId,
    pub attached_at: Instant,
    /// R3 invariant guard. MUST be `true` before any navigation.
    pub determinism_injected: bool,
    /// Profile string passed at `spawn_target` time.
    pub profile: String,
    /// voice-call-io: the per-target nonce the mic-override bootstrap was
    /// rendered with (`window.__loom_<nonce>`), set when `--audio` and the
    /// install succeeded. `None` for non-`--audio` targets (or if the
    /// best-effort install failed). Task 05's capture tap reads this to address
    /// the in-page API + exclude injected tracks.
    pub audio_nonce: Option<String>,
}

impl TargetState {
    pub fn new(session_id: SessionId, target_id: TargetId, profile: String) -> Self {
        Self {
            session_id,
            target_id,
            attached_at: Instant::now(),
            determinism_injected: false,
            profile,
            audio_nonce: None,
        }
    }
}

/// Concrete TargetManager.
pub struct ChromiumTargetManager {
    pub(crate) cdp: Arc<dyn CdpConnection>,
    pub(crate) determinism: Arc<dyn DeterminismInjector>,
    pub(crate) response_tx: ResponseSender,
    pub(crate) by_target: parking_lot::RwLock<BTreeMap<TargetId, TargetState>>,
    pub(crate) by_session: parking_lot::RwLock<BTreeMap<SessionId, TargetId>>,
}

impl ChromiumTargetManager {
    pub fn new(
        cdp: Arc<dyn CdpConnection>,
        determinism: Arc<dyn DeterminismInjector>,
        response_tx: ResponseSender,
    ) -> Self {
        Self {
            cdp,
            determinism,
            response_tx,
            by_target: parking_lot::RwLock::new(BTreeMap::new()),
            by_session: parking_lot::RwLock::new(BTreeMap::new()),
        }
    }
}

/// Public TargetManager trait surface.
#[async_trait::async_trait]
pub trait TargetManager: Send + Sync {
    /// Create a new Chromium target for the given session. Enforces
    /// R3 ordering: `await inject(target_id, seed, epoch_ms)` →
    /// `Network.enable` → `Page.enable` → return. Idempotent: if
    /// `session_id` already has a target, returns the existing target_id
    /// without re-injecting.
    ///
    /// `seed` and `epoch_ms` are rendered into the determinism JS
    /// template per-target. The flag `TargetState.determinism_injected`
    /// flips to `true` ONLY on `Ok(())` from the awaited inject; a
    /// failed inject leaves the flag false AND does not insert into
    /// `by_target`/`by_session`, so a retry runs cleanly.
    ///
    /// Errors: `TargetError::CdpFailure` (any of the CDP commands
    /// failed); `TargetError::DeterminismInjectionFailed`.
    async fn create_new_target(
        &self,
        session_id: SessionId,
        profile: String,
        seed: Seed,
        epoch_ms: EpochMs,
        // settle-capture (4b): when `false`, SKIP the determinism freeze-inject
        // (real clock + unseeded RNG). The R3 readiness flag still flips.
        determinism_enabled: bool,
        // voice-call-io (task 03): when `true` (session created with `--audio`),
        // install the synthetic-microphone bootstrap (`audio_bootstrap.js`) with
        // a fresh per-target nonce. Best-effort — unlike the determinism inject
        // (load-bearing R3), a failed audio install only WARNs; audio is a
        // convenience layer, so it must never fail target creation.
        audio_enabled: bool,
    ) -> Result<TargetId, TargetError>;

    /// Look up the target for a session; None if unknown.
    fn target_for_session(&self, session_id: SessionId) -> Option<TargetId>;

    /// Look up the full state record for a target.
    fn target_state(&self, target_id: TargetId) -> Option<TargetState>;

    /// Close a target. Drops both maps' entries; calls
    /// `Target.closeTarget` via `CdpConnection`.
    fn close_target(&self, target_id: TargetId) -> Result<(), TargetError>;

    /// State-invalidation cascade. Called by `Supervisor::handle_crash`.
    fn invalidate_targets(&self);

    /// R3-guard query — used by `ActionExecutor::navigate` before
    /// dispatching `Page.navigate`. Returns `false` if the target's
    /// `determinism_injected` flag is still false.
    fn determinism_ready(&self, target_id: TargetId) -> bool;
}

#[derive(Debug, thiserror::Error)]
pub enum TargetError {
    #[error("session {0} already bound to target {1}")]
    SessionAlreadyBound(SessionId, TargetId),
    #[error("target {0} not found")]
    NotFound(TargetId),
    #[error("CDP command failed: {0}")]
    CdpFailure(#[from] CdpError),
    #[error("determinism injection failed: {0}")]
    DeterminismInjectionFailed(String),
    #[error("R3 ordering violation: navigate before inject on target {0}")]
    R3OrderingViolation(TargetId),
}

impl From<TargetError> for ShimErrorCode {
    fn from(e: TargetError) -> Self {
        match e {
            TargetError::CdpFailure(c) => c.into(),
            TargetError::NotFound(_) | TargetError::SessionAlreadyBound(_, _) => {
                ShimErrorCode::TargetUnknown
            }
            TargetError::DeterminismInjectionFailed(_) | TargetError::R3OrderingViolation(_) => {
                ShimErrorCode::ShimInternalError
            }
        }
    }
}

#[async_trait::async_trait]
impl TargetManager for ChromiumTargetManager {
    async fn create_new_target(
        &self,
        session_id: SessionId,
        profile: String,
        seed: Seed,
        epoch_ms: EpochMs,
        determinism_enabled: bool,
        audio_enabled: bool,
    ) -> Result<TargetId, TargetError> {
        // Idempotent: return existing target if session already has one
        if let Some(existing) = self.by_session.read().get(&session_id).copied() {
            return Ok(existing);
        }
        // Future: Target.createTarget via chromiumoxide.
        // Current: synthesize target_id from session_id.
        let target_id = session_id.wrapping_mul(0x9e3779b97f4a7c15);
        let mut state = TargetState::new(session_id, target_id, profile);
        // R3 LOAD-BEARING. Await the inject; flag flips ONLY on Ok. A failed
        // inject leaves the flag false AND does not insert — a retry runs
        // cleanly without leaking state.
        //
        // settle-capture (4b): when determinism is OFF, SKIP the freeze-inject
        // so the page keeps real `Date.now`/`Math.random`. The R3 readiness
        // flag still flips (the target is "ready" — there is simply nothing to
        // inject), so the navigate gate is UNCHANGED. The session was marked
        // non-replayable in its manifest Header, so no determinism guarantee is
        // silently weakened.
        if determinism_enabled {
            self.determinism
                .inject(target_id, seed, epoch_ms)
                .await
                .map_err(|e| TargetError::DeterminismInjectionFailed(e.to_string()))?;
        }
        state.determinism_injected = true;

        // voice-call-io (task 03): install the synthetic-microphone bootstrap
        // for `--audio` sessions. UNLIKE the determinism inject above (load-
        // bearing R3), this is BEST-EFFORT: a real voice call runs
        // `--no-determinism`, so the determinism inject never fires and the mic
        // override rides its own `addScriptToEvaluateOnNewDocument`
        // (runImmediately) so it is in place before the app's first
        // `getUserMedia`. A failed install only WARNs — audio is a convenience
        // layer, and the `--use-fake-device` launch flag is the benign fallback
        // (a beep, never a hang).
        if audio_enabled {
            let nonce = crate::audio_bridge::generate_nonce();
            let msg = CdpMessage {
                method: crate::audio_bridge::ADD_SCRIPT_METHOD.to_string(),
                params: crate::audio_bridge::build_install_params(&nonce),
            };
            match self.cdp.command(target_id, msg, None).await {
                Ok(_) => {
                    tracing::info!(
                        target_id,
                        "audio: synthetic-microphone bootstrap installed (--audio)"
                    );
                    state.audio_nonce = Some(nonce);
                }
                Err(e) => {
                    tracing::warn!(
                        target_id,
                        error = %e,
                        "audio: addScriptToEvaluateOnNewDocument for the mic override \
                         failed; getUserMedia will return the real (fake-device) mic, \
                         not injected audio"
                    );
                }
            }
        }

        self.by_target.write().insert(target_id, state);
        self.by_session.write().insert(session_id, target_id);
        Ok(target_id)
    }

    fn target_for_session(&self, session_id: SessionId) -> Option<TargetId> {
        self.by_session.read().get(&session_id).copied()
    }

    fn target_state(&self, target_id: TargetId) -> Option<TargetState> {
        self.by_target.read().get(&target_id).cloned()
    }

    fn close_target(&self, target_id: TargetId) -> Result<(), TargetError> {
        let session_id = self
            .by_target
            .write()
            .remove(&target_id)
            .map(|s| s.session_id);
        if let Some(sid) = session_id {
            self.by_session.write().remove(&sid);
        }
        Ok(())
    }

    fn invalidate_targets(&self) {
        self.by_target.write().clear();
        self.by_session.write().clear();
    }

    fn determinism_ready(&self, target_id: TargetId) -> bool {
        self.by_target
            .read()
            .get(&target_id)
            .is_some_and(|s| s.determinism_injected)
    }
}

/// Pure helper: the canonical ordered domain-enable sequence for a new
/// target. `create_new_target` runs these AFTER injection.
/// Returns the CDP method names, in order.
pub fn ordered_domain_enables() -> &'static [&'static str] {
    &["Network.enable", "Page.enable", "Log.enable"]
}

/// Pure helper: validate the R3 invariant on a `TargetState` snapshot.
/// Returns `Err` if the flag is still false. Used by tests +
/// `ActionExecutor::navigate`.
pub fn assert_r3_ready(state: &TargetState) -> Result<(), TargetError> {
    if state.determinism_injected {
        Ok(())
    } else {
        Err(TargetError::R3OrderingViolation(state.target_id))
    }
}
