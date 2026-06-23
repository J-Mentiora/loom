// ShimManager — typed per-verb senders.
//
// One `impl ShimManager` block carrying the typed verb senders that wrap a
// `ShimRequest` round-trip and decode the response into a domain outcome:
// `send_navigate`, `send_network_log`, `send_start_recording`,
// `send_stop_recording`, `send_wait_for`, `send_evaluate`, and
// `send_set_input_files`. Split out of `shim_manager.rs` (behavior-preserving).
//
// The four formerly-`#[allow(clippy::too_many_arguments)]` senders now take a
// dedicated params struct (see `super::types`); the generic `send` and the
// lifecycle / breaker methods stay in `shim_manager.rs`.

use super::helpers::{cbor_get, cbor_u64, map_shim_code, parse_evaluate_payload, shim_error_class};
use super::input_dispatch::{
    fill_events, keystroke_events_for_text, mouse_event, press_key_events,
};
use super::process::send_and_await;
use super::shim_manager::ShimManager;
use super::types::{
    EvaluateOutcome, FailureClass, InputDispatchOutcome, SendEvaluateParams, SendNavigateParams,
    SendPressKeyParams, SendSetInputFilesParams, SendWaitForParams, SetInputFilesOutcome, ShimId,
};
use loom_core::error::{LoomError, LoomErrorCode};
use loom_shared::locator::{parse_locator, Segment};
use loom_shared::navigate_outcome::{NavigateOutcome, NetworkLogOutcome, ScreencastOutcome};
use loom_shared::shim_protocol::{CdpMessage, ShimErrorCode, ShimRequest, ShimResponse};
use std::time::Duration;

impl ShimManager {
    /// Send a typed PageNavigate request and decode the response as
    /// `NavigateOutcome`. Uses `ShimRequest::PageNavigate` (not CdpSend).
    /// See [`SendNavigateParams`] for the per-call fields (`budget_ms`
    /// overrides the recv timeout when larger than the default; `action_id`
    /// is the WASM-guest-computed action hash threaded for host-side receipt
    /// correlation / observability — the shim does not see it; `seed` /
    /// `epoch_ms` ride the wire to the shim's determinism JS template;
    /// `blocklist_enabled` toggles the shim's `Fetch.enable` interception
    /// path).
    pub async fn send_navigate(
        &self,
        params: SendNavigateParams,
    ) -> Result<NavigateOutcome, LoomError> {
        let SendNavigateParams {
            id,
            action_id,
            session_id,
            target_id,
            url,
            budget_ms,
            seed,
            epoch_ms,
            blocklist_enabled,
            until,
            determinism_enabled,
        } = params;
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
                loom_shared::shim_protocol::ciborium_from_slice::<NavigateOutcome>(&bytes).map_err(
                    |e| {
                        LoomError::new(
                            LoomErrorCode::ShimFailure,
                            format!("shim {}: navigate outcome decode: {e}", id.0),
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
                loom_shared::shim_protocol::ciborium_from_slice::<NetworkLogOutcome>(&bytes)
                    .map_err(|e| {
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

    /// video-capture: start a screencast recording on the target
    /// (`ShimRequest::StartRecording`). Returns `Ok(())` on confirmation; a
    /// recording-already-active / startScreencast failure surfaces as a typed
    /// `LoomError`.
    pub async fn send_start_recording(
        &self,
        id: ShimId,
        session_id: u64,
        target_id: u64,
        max_duration_ms: u64,
        max_bytes: u64,
        frame_rate: u32,
    ) -> Result<(), LoomError> {
        self.check_breaker(&id)?;
        let config = self.configs.get(&id).map(|c| c.clone()).ok_or_else(|| {
            LoomError::new(
                LoomErrorCode::ShimFailure,
                format!("shim {} not registered", id.0),
            )
        })?;
        let process = self.get_or_spawn(&id, &config).await?;
        let request = ShimRequest::StartRecording {
            request_id: 0,
            session_id,
            target_id,
            max_duration_ms,
            max_bytes,
            frame_rate,
        };
        match send_and_await(
            &process,
            request,
            Duration::from_millis(config.send_timeout_ms),
            Duration::from_millis(config.recv_timeout_ms),
        )
        .await
        {
            Ok(ShimResponse::Ok { .. }) => {
                self.record_success(&id);
                Ok(())
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

    /// video-capture: stop the active recording (`ShimRequest::StopRecording`)
    /// and return the decoded `ScreencastOutcome` (the host then writes the
    /// `webm_bytes` to CAS). A LONGER receive timeout is used because the shim
    /// runs the ffmpeg encode synchronously before responding.
    pub async fn send_stop_recording(
        &self,
        id: ShimId,
        session_id: u64,
        target_id: u64,
    ) -> Result<ScreencastOutcome, LoomError> {
        self.check_breaker(&id)?;
        let config = self.configs.get(&id).map(|c| c.clone()).ok_or_else(|| {
            LoomError::new(
                LoomErrorCode::ShimFailure,
                format!("shim {} not registered", id.0),
            )
        })?;
        let process = self.get_or_spawn(&id, &config).await?;
        let request = ShimRequest::StopRecording {
            request_id: 0,
            session_id,
            target_id,
        };
        // Encoding can take seconds for a multi-minute recording; give the
        // recv a generous floor independent of the per-CDP-command budget.
        let recv_timeout = Duration::from_millis(config.recv_timeout_ms.max(120_000));
        match send_and_await(
            &process,
            request,
            Duration::from_millis(config.send_timeout_ms),
            recv_timeout,
        )
        .await
        {
            Ok(ShimResponse::Ok { payload, .. }) => {
                self.record_success(&id);
                let mut bytes = Vec::new();
                if let Err(e) = ciborium::ser::into_writer(&payload, &mut bytes) {
                    return Err(LoomError::new(
                        LoomErrorCode::ShimFailure,
                        format!("shim {}: stop_recording response re-encode: {e}", id.0),
                    ));
                }
                loom_shared::shim_protocol::ciborium_from_slice::<ScreencastOutcome>(&bytes)
                    .map_err(|e| {
                        LoomError::new(
                            LoomErrorCode::ShimFailure,
                            format!("shim {}: screencast outcome decode: {e}", id.0),
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
    /// bootstrap about:blank), then the typed wait request. See
    /// [`SendWaitForParams`] for the per-call fields.
    pub async fn send_wait_for(
        &self,
        params: SendWaitForParams,
    ) -> Result<loom_shared::navigate_outcome::WaitOutcome, LoomError> {
        let SendWaitForParams {
            id,
            action_id,
            session_id,
            target_id,
            until,
            budget_ms,
            seed,
            epoch_ms,
            determinism_enabled,
        } = params;
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
                loom_shared::shim_protocol::ciborium_from_slice::<
                    loom_shared::navigate_outcome::WaitOutcome,
                >(&bytes)
                .map_err(|e| {
                    LoomError::new(
                        LoomErrorCode::ShimFailure,
                        format!("shim {}: wait_for outcome decode: {e}", id.0),
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

    /// Send `Runtime.evaluate` against `id`'s target via CdpSend and parse
    /// the response into a typed `EvaluateOutcome`. See [`SendEvaluateParams`]
    /// for the per-call fields (`action_id` is the WASM-guest-computed action
    /// hash threaded for receipt correlation; not sent to the shim).
    ///
    /// CDP `Runtime.evaluate` shape:
    ///   request:  {expression, returnByValue:true, awaitPromise:true}
    ///   response: { result: {type, value?}, exceptionDetails?: {...} }
    /// On exceptionDetails the host wraps as HostError::ShimFailure with
    /// `{kind:"js_throw", exception:..., line:..., column:...}` JSON.
    pub async fn send_evaluate(
        &self,
        params: SendEvaluateParams,
    ) -> Result<EvaluateOutcome, LoomError> {
        let SendEvaluateParams {
            id,
            action_id,
            session_id,
            target_id,
            expression,
            budget_ms,
            seed,
            epoch_ms,
            determinism_enabled,
        } = params;
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
    /// canonicalized daemon-side (upload_guard) before reaching here. See
    /// [`SendSetInputFilesParams`] for the per-call fields.
    ///
    /// Outcomes:
    ///   - `Ok(SetInputFilesOutcome::Ok { file_count })` on success.
    ///   - `Ok(SelectorNotFound)` when querySelector returns nodeId == 0.
    ///   - `Ok(NotAFileInput)` when setFileInputFiles errors on a resolved node.
    ///   - `Err(LoomError)` for transport / breaker / protocol failures.
    pub async fn send_set_input_files(
        &self,
        params: SendSetInputFilesParams,
    ) -> Result<SetInputFilesOutcome, LoomError> {
        use ciborium::value::{Integer, Value};
        let SendSetInputFilesParams {
            id,
            action_id,
            session_id,
            target_id,
            selector,
            files,
            budget_ms,
            seed,
            epoch_ms,
            determinism_enabled,
        } = params;
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

    // ─── Trusted CDP input dispatch (cdp-trusted-input) ──────────────────────
    //
    // `web.type mode:keystrokes`, `web.press_key`, and the always-trusted
    // `web.click` drive REAL (`isTrusted:true`) browser input via CDP `Input.*`.
    // Each orchestrates a multi-step CDP sequence host-side from raw `CdpSend`
    // round-trips — the same shape as `send_set_input_files`, so the shim and
    // wire protocol are untouched.

    /// One CDP round-trip. `Ok(Ok(payload))` = CDP success; `Ok(Err((code,
    /// detail)))` = the shim reported a CDP-protocol error (an APPLICATION
    /// outcome the caller interprets, e.g. `getBoxModel` on a hidden node);
    /// `Err(LoomError)` = transport failure. Breaker bookkeeping is done by the
    /// caller at its decision points.
    async fn cdp_send_one(
        &self,
        id: &ShimId,
        session_id: u64,
        target_id: u64,
        message: CdpMessage,
        budget_ms: u64,
    ) -> Result<Result<ciborium::value::Value, (ShimErrorCode, String)>, LoomError> {
        let config = self.configs.get(id).map(|c| c.clone()).ok_or_else(|| {
            LoomError::new(
                LoomErrorCode::ShimFailure,
                format!("shim {} not registered", id.0),
            )
        })?;
        let process = self.get_or_spawn(id, &config).await?;
        let recv_ms = budget_ms.max(config.recv_timeout_ms);
        match send_and_await(
            &process,
            ShimRequest::CdpSend {
                request_id: 0,
                session_id,
                target_id,
                message,
            },
            Duration::from_millis(config.send_timeout_ms),
            Duration::from_millis(recv_ms),
        )
        .await
        {
            Ok(ShimResponse::Ok { payload, .. }) => Ok(Ok(payload)),
            Ok(ShimResponse::Error { code, detail, .. }) => Ok(Err((code, detail))),
            Ok(other) => Err(LoomError::new(
                LoomErrorCode::ShimFailure,
                format!("shim {}: unexpected CDP response: {other:?}", id.0),
            )),
            Err(e) => Err(e),
        }
    }

    /// Resolve `selector` to a node and focus it. `Ok(true)` focused; `Ok(false)`
    /// selector matched nothing; `Err` on transport failure. Focus itself is
    /// best-effort (a non-focusable node still receives dispatched key events).
    async fn resolve_and_focus(
        &self,
        id: &ShimId,
        session_id: u64,
        target_id: u64,
        selector: &str,
        budget_ms: u64,
    ) -> Result<bool, LoomError> {
        use ciborium::value::{Integer, Value};
        // Frame-aware resolution (descends same-process cross-origin iframes);
        // for a bare/plain CSS selector this is the same getDocument →
        // querySelector path as before.
        let node = match self
            .resolve_locator_node(id, session_id, target_id, selector, budget_ms)
            .await?
        {
            Some(n) => n,
            None => return Ok(false),
        };
        // Best-effort focus — ignore a CDP error (non-focusable element).
        // `Input.insertText`/`dispatchKeyEvent` then target the focused element,
        // which is correct even when it lives inside a cross-origin frame.
        let _ = self
            .cdp_send_one(
                id,
                session_id,
                target_id,
                CdpMessage {
                    method: "DOM.focus".into(),
                    params: Value::Map(vec![(
                        Value::Text("nodeId".into()),
                        Value::Integer(Integer::from(node)),
                    )]),
                },
                budget_ms,
            )
            .await?;
        Ok(true)
    }

    /// Dispatch a prebuilt sequence of `Input.*` frames; any CDP/transport error
    /// aborts (no partial "ok"). Used by the keystroke + press-key paths.
    async fn dispatch_input_events(
        &self,
        id: &ShimId,
        session_id: u64,
        target_id: u64,
        events: Vec<CdpMessage>,
        budget_ms: u64,
    ) -> Result<(), LoomError> {
        for ev in events {
            self.cdp_send_one(id, session_id, target_id, ev, budget_ms)
                .await?
                .map_err(|(code, detail)| {
                    LoomError::new(
                        map_shim_code(code),
                        format!("shim {}: input dispatch: {detail}", id.0),
                    )
                })?;
        }
        Ok(())
    }

    /// `web.type mode:keystrokes` — focus `selector`, then send a real per-char
    /// `Input.dispatchKeyEvent` (keyDown+text → keyUp) sequence.
    pub async fn send_type_keystrokes(
        &self,
        id: ShimId,
        session_id: u64,
        target_id: u64,
        selector: String,
        text: String,
        budget_ms: u64,
    ) -> Result<InputDispatchOutcome, LoomError> {
        self.check_breaker(&id)?;
        if !self
            .resolve_and_focus(&id, session_id, target_id, &selector, budget_ms)
            .await
            .inspect_err(|_| self.record_failure(&id, FailureClass::Transport))?
        {
            self.record_success(&id);
            return Ok(InputDispatchOutcome::SelectorNotFound);
        }
        match self
            .dispatch_input_events(
                &id,
                session_id,
                target_id,
                keystroke_events_for_text(&text),
                budget_ms,
            )
            .await
        {
            Ok(()) => {
                self.record_success(&id);
                Ok(InputDispatchOutcome::Ok)
            }
            Err(e) => {
                self.record_failure(&id, FailureClass::Application);
                Err(e)
            }
        }
    }

    /// `web.type` DEFAULT (`mode:"fill"`) — focus `selector`, then drive the value
    /// through CDP `Input.insertText` (Playwright `fill()` semantics): select the
    /// existing content and commit `text` as one GENUINE (`isTrusted:true`) edit so
    /// React/react-hook-form `onChange` fires AND the value is treated as
    /// user-entered. Same selector-resolution + breaker bookkeeping as
    /// `send_type_keystrokes`; differs only in the dispatched frames.
    pub async fn send_type_fill(
        &self,
        id: ShimId,
        session_id: u64,
        target_id: u64,
        selector: String,
        text: String,
        budget_ms: u64,
    ) -> Result<InputDispatchOutcome, LoomError> {
        self.check_breaker(&id)?;
        if !self
            .resolve_and_focus(&id, session_id, target_id, &selector, budget_ms)
            .await
            .inspect_err(|_| self.record_failure(&id, FailureClass::Transport))?
        {
            self.record_success(&id);
            return Ok(InputDispatchOutcome::SelectorNotFound);
        }
        match self
            .dispatch_input_events(
                &id,
                session_id,
                target_id,
                fill_events(&selector, &text),
                budget_ms,
            )
            .await
        {
            Ok(()) => {
                self.record_success(&id);
                Ok(InputDispatchOutcome::Ok)
            }
            Err(e) => {
                self.record_failure(&id, FailureClass::Application);
                Err(e)
            }
        }
    }

    /// `web.press_key` — optionally focus `selector`, then dispatch a named key
    /// (+ modifier combo) as real `Input.dispatchKeyEvent` frames. An unknown
    /// key / modifier is the typed `UnknownKey` outcome (not a transport error).
    pub async fn send_press_key(
        &self,
        params: SendPressKeyParams,
    ) -> Result<InputDispatchOutcome, LoomError> {
        let SendPressKeyParams {
            id,
            session_id,
            target_id,
            key,
            selector,
            modifiers,
            budget_ms,
        } = params;
        self.check_breaker(&id)?;
        if let Some(sel) = &selector {
            if !self
                .resolve_and_focus(&id, session_id, target_id, sel, budget_ms)
                .await
                .inspect_err(|_| self.record_failure(&id, FailureClass::Transport))?
            {
                self.record_success(&id);
                return Ok(InputDispatchOutcome::SelectorNotFound);
            }
        }
        let events = match press_key_events(&key, &modifiers) {
            Some(e) => e,
            None => {
                self.record_success(&id);
                return Ok(InputDispatchOutcome::UnknownKey);
            }
        };
        match self
            .dispatch_input_events(&id, session_id, target_id, events, budget_ms)
            .await
        {
            Ok(()) => {
                self.record_success(&id);
                Ok(InputDispatchOutcome::Ok)
            }
            Err(e) => {
                self.record_failure(&id, FailureClass::Application);
                Err(e)
            }
        }
    }

    /// Always-trusted `web.click` — resolve the element's hit point (box-model
    /// center, scrolling into view first) and dispatch a trusted
    /// `DOM.querySelector(root, css)` → `Some(nodeId)` (a 0 nodeId ⇒ `None`).
    async fn dom_query_selector(
        &self,
        id: &ShimId,
        session_id: u64,
        target_id: u64,
        root: u64,
        css: &str,
        budget_ms: u64,
    ) -> Result<Option<u64>, LoomError> {
        use ciborium::value::{Integer, Value};
        let qs = self
            .cdp_send_one(
                id,
                session_id,
                target_id,
                CdpMessage {
                    method: "DOM.querySelector".into(),
                    params: Value::Map(vec![
                        (
                            Value::Text("nodeId".into()),
                            Value::Integer(Integer::from(root)),
                        ),
                        (Value::Text("selector".into()), Value::Text(css.to_string())),
                    ]),
                },
                budget_ms,
            )
            .await?
            .map_err(|(code, detail)| {
                LoomError::new(map_shim_code(code), format!("shim {}: {detail}", id.0))
            })?;
        let node = cbor_get(&qs, "nodeId").and_then(cbor_u64).unwrap_or(0);
        Ok(if node == 0 { None } else { Some(node) })
    }

    /// Resolve a (possibly `frame=`-prefixed) locator to a DOM `nodeId` in the
    /// page session, descending through same-process iframes (incl. same-site
    /// cross-origin) via `DOM.describeNode{pierce:true}` → `contentDocument`.
    /// CDP is not bound by the same-origin policy, so a cross-origin (but
    /// in-process) frame's content is reachable this way — the
    /// `iframe.contentDocument === null` blocker is a page-JS limitation, not a
    /// CDP one.
    ///
    /// Returns `Ok(Some(nodeId))` on a match; `Ok(None)` when nothing matched, the
    /// frame is out-of-process (no in-process `contentDocument`), or the leaf is a
    /// `text=`/`role=` form (resolved by the evaluate-tier resolver, not this DOM
    /// path). `Err` only on transport failure.
    pub(super) async fn resolve_locator_node(
        &self,
        id: &ShimId,
        session_id: u64,
        target_id: u64,
        selector: &str,
        budget_ms: u64,
    ) -> Result<Option<u64>, LoomError> {
        use ciborium::value::{Integer, Value};

        let segments = match parse_locator(selector) {
            Ok(s) => s,
            Err(_) => return Ok(None),
        };

        // A single `text=`/`role=` locator resolves in the top frame's default
        // execution context via a marker-attribute resolver (the common
        // testid-less case, e.g. a shadcn button). Composed (`… >> text=`) and
        // in-(cross-origin-)frame text/role are a follow-up — they need the
        // frame's executionContextId.
        if let [seg @ (Segment::Text(_) | Segment::Role(_))] = segments.as_slice() {
            return self
                .resolve_marked_node(id, session_id, target_id, seg, budget_ms)
                .await;
        }

        let doc = self
            .cdp_send_one(
                id,
                session_id,
                target_id,
                CdpMessage {
                    method: "DOM.getDocument".into(),
                    params: Value::Map(vec![(
                        Value::Text("depth".into()),
                        Value::Integer(Integer::from(0)),
                    )]),
                },
                budget_ms,
            )
            .await?
            .map_err(|(code, detail)| {
                LoomError::new(map_shim_code(code), format!("shim {}: {detail}", id.0))
            })?;
        let mut root = cbor_get(&doc, "root")
            .and_then(|r| cbor_get(r, "nodeId"))
            .and_then(cbor_u64)
            .ok_or_else(|| {
                LoomError::new(
                    LoomErrorCode::ShimFailure,
                    format!("shim {}: getDocument: no root.nodeId", id.0),
                )
            })?;

        let last = segments.len() - 1;
        for (i, seg) in segments.iter().enumerate() {
            let is_last = i == last;
            match seg {
                Segment::Frame(css) => {
                    let iframe = match self
                        .dom_query_selector(id, session_id, target_id, root, css, budget_ms)
                        .await?
                    {
                        Some(n) => n,
                        None => return Ok(None),
                    };
                    let described = self
                        .cdp_send_one(
                            id,
                            session_id,
                            target_id,
                            CdpMessage {
                                method: "DOM.describeNode".into(),
                                params: Value::Map(vec![
                                    (
                                        Value::Text("nodeId".into()),
                                        Value::Integer(Integer::from(iframe)),
                                    ),
                                    (
                                        Value::Text("depth".into()),
                                        Value::Integer(Integer::from(-1i64)),
                                    ),
                                    (Value::Text("pierce".into()), Value::Bool(true)),
                                ]),
                            },
                            budget_ms,
                        )
                        .await?
                        .map_err(|(code, detail)| {
                            LoomError::new(map_shim_code(code), format!("shim {}: {detail}", id.0))
                        })?;
                    match cbor_get(&described, "node")
                        .and_then(|n| cbor_get(n, "contentDocument"))
                        .and_then(|cd| cbor_get(cd, "nodeId"))
                        .and_then(cbor_u64)
                    {
                        Some(n) if n != 0 => root = n,
                        // No in-process contentDocument ⇒ out-of-process (OOPIF)
                        // frame; not handled on this DOM path.
                        _ => return Ok(None),
                    }
                }
                Segment::Css(css) => {
                    match self
                        .dom_query_selector(id, session_id, target_id, root, css, budget_ms)
                        .await?
                    {
                        Some(n) if is_last => return Ok(Some(n)),
                        Some(n) => root = n, // intermediate css scope
                        None => return Ok(None),
                    }
                }
                // text=/role= are resolved by the evaluate-tier resolver, not
                // this DOM path.
                Segment::Text(_) | Segment::Role(_) => return Ok(None),
            }
        }
        // Ended on a `frame=` segment with no leaf target.
        Ok(None)
    }

    /// Resolve a single `text=`/`role=` segment in the top frame's default
    /// execution context: run a marker-attribute resolver (W3C-AccName subset
    /// for `role=`; visible-text match for `text=`), then `querySelector` the
    /// marked node and strip the marker. Returns the matched `nodeId` or `None`.
    async fn resolve_marked_node(
        &self,
        id: &ShimId,
        session_id: u64,
        target_id: u64,
        seg: &Segment,
        budget_ms: u64,
    ) -> Result<Option<u64>, LoomError> {
        use ciborium::value::{Integer, Value};
        let js = match marker_resolver_js(seg) {
            Some(js) => js,
            None => return Ok(None),
        };
        let eval = |expr: String| CdpMessage {
            method: "Runtime.evaluate".into(),
            params: Value::Map(vec![
                (Value::Text("expression".into()), Value::Text(expr)),
                (Value::Text("returnByValue".into()), Value::Bool(true)),
            ]),
        };
        let resp = self
            .cdp_send_one(id, session_id, target_id, eval(js), budget_ms)
            .await?
            .map_err(|(code, detail)| {
                LoomError::new(map_shim_code(code), format!("shim {}: {detail}", id.0))
            })?;
        let found = cbor_get(&resp, "result")
            .and_then(|r| cbor_get(r, "value"))
            .map(|v| matches!(v, Value::Bool(true)))
            .unwrap_or(false);
        if !found {
            return Ok(None);
        }
        let doc = self
            .cdp_send_one(
                id,
                session_id,
                target_id,
                CdpMessage {
                    method: "DOM.getDocument".into(),
                    params: Value::Map(vec![(
                        Value::Text("depth".into()),
                        Value::Integer(Integer::from(0)),
                    )]),
                },
                budget_ms,
            )
            .await?
            .map_err(|(code, detail)| {
                LoomError::new(map_shim_code(code), format!("shim {}: {detail}", id.0))
            })?;
        let root = cbor_get(&doc, "root")
            .and_then(|r| cbor_get(r, "nodeId"))
            .and_then(cbor_u64)
            .ok_or_else(|| {
                LoomError::new(
                    LoomErrorCode::ShimFailure,
                    format!("shim {}: getDocument: no root.nodeId", id.0),
                )
            })?;
        let node = self
            .dom_query_selector(id, session_id, target_id, root, MARKER_SELECTOR, budget_ms)
            .await?;
        // Best-effort: strip the marker so it does not linger in the DOM.
        let _ = self
            .cdp_send_one(
                id,
                session_id,
                target_id,
                eval(format!(
                    "document.querySelectorAll('{MARKER_SELECTOR}').forEach(function(e){{e.removeAttribute('{MARKER_ATTR}');}})"
                )),
                budget_ms,
            )
            .await;
        Ok(node)
    }

    /// `Input.dispatchMouseEvent` mouseMoved→mousePressed→mouseReleased. No
    /// `el.click()` fallback. `SelectorNotFound` / `NotHittable` are typed
    /// outcomes; transport failures surface as `Err`.
    pub async fn send_trusted_click(
        &self,
        id: ShimId,
        session_id: u64,
        target_id: u64,
        selector: String,
        budget_ms: u64,
    ) -> Result<InputDispatchOutcome, LoomError> {
        use ciborium::value::{Integer, Value};
        self.check_breaker(&id)?;

        // Resolve the (possibly `frame=`-prefixed) locator to a node id,
        // descending into same-process iframes for cross-origin reach.
        let node = match self
            .resolve_locator_node(&id, session_id, target_id, &selector, budget_ms)
            .await
            .inspect_err(|_| self.record_failure(&id, FailureClass::Transport))?
        {
            Some(n) => n,
            None => {
                self.record_success(&id);
                return Ok(InputDispatchOutcome::SelectorNotFound);
            }
        };

        // Scroll into view (best-effort) before resolving coordinates.
        let _ = self
            .cdp_send_one(
                &id,
                session_id,
                target_id,
                CdpMessage {
                    method: "DOM.scrollIntoViewIfNeeded".into(),
                    params: Value::Map(vec![(
                        Value::Text("nodeId".into()),
                        Value::Integer(Integer::from(node)),
                    )]),
                },
                budget_ms,
            )
            .await?;

        // Box model → content-quad center. A CDP error here means the element
        // has no box model (display:none / detached) → NotHittable.
        let box_payload = match self
            .cdp_send_one(
                &id,
                session_id,
                target_id,
                CdpMessage {
                    method: "DOM.getBoxModel".into(),
                    params: Value::Map(vec![(
                        Value::Text("nodeId".into()),
                        Value::Integer(Integer::from(node)),
                    )]),
                },
                budget_ms,
            )
            .await?
        {
            Ok(p) => p,
            Err(_) => {
                self.record_success(&id);
                return Ok(InputDispatchOutcome::NotHittable);
            }
        };
        let (cx, cy) = match content_quad_center(&box_payload) {
            Some(c) => c,
            None => {
                self.record_success(&id);
                return Ok(InputDispatchOutcome::NotHittable);
            }
        };

        // Trusted click: mouseMoved → mousePressed → mouseReleased at (cx, cy).
        let events = vec![
            mouse_event("mouseMoved", cx, cy, "none", 0),
            mouse_event("mousePressed", cx, cy, "left", 1),
            mouse_event("mouseReleased", cx, cy, "left", 1),
        ];
        match self
            .dispatch_input_events(&id, session_id, target_id, events, budget_ms)
            .await
        {
            Ok(()) => {
                self.record_success(&id);
                Ok(InputDispatchOutcome::Ok)
            }
            Err(e) => {
                self.record_failure(&id, FailureClass::Application);
                Err(e)
            }
        }
    }
}

/// Center of a CDP `DOM.getBoxModel` content quad. `content` is
/// `[x1,y1,x2,y2,x3,y3,x4,y4]`; center = midpoint of opposite corners
/// (1 and 3). `None` when the payload lacks a usable quad.
fn content_quad_center(payload: &ciborium::value::Value) -> Option<(i64, i64)> {
    use ciborium::value::Value;
    let num = |v: &Value| -> Option<f64> {
        match v {
            Value::Float(f) => Some(*f),
            Value::Integer(i) => i64::try_from(*i).ok().map(|n| n as f64),
            _ => None,
        }
    };
    let content = cbor_get(payload, "model").and_then(|m| cbor_get(m, "content"))?;
    if let Value::Array(pts) = content {
        if pts.len() >= 6 {
            let x1 = num(&pts[0])?;
            let y1 = num(&pts[1])?;
            let x3 = num(&pts[4])?;
            let y3 = num(&pts[5])?;
            return Some((
                ((x1 + x3) / 2.0).round() as i64,
                ((y1 + y3) / 2.0).round() as i64,
            ));
        }
    }
    None
}

// ─── text=/role= marker resolver (P2, top frame) ─────────────────────────────
//
// We mark the matched element with a transient attribute and `querySelector` it
// (rather than returning coords) so the existing nodeId-based click/focus paths
// are reused unchanged. The marker is stripped after resolution. Resolution
// happens at record time only; replay is structural, so the marker never enters
// the hash chain.

const MARKER_ATTR: &str = "data-loom-loc";
const MARKER_SELECTOR: &str = "[data-loom-loc]";

/// Shared JS: clear any stale marker, plus `vis()` (visible: non-zero box and no
/// display:none/visibility:hidden) and `norm()` (collapse whitespace + trim).
const JS_PRELUDE: &str = "var M='data-loom-loc';document.querySelectorAll('['+M+']').forEach(function(e){e.removeAttribute(M);});function vis(e){var r=e.getBoundingClientRect();if(r.width===0&&r.height===0)return false;var s=getComputedStyle(e);return s.display!=='none'&&s.visibility!=='hidden';}function norm(t){return (t||'').replace(/\\s+/g,' ').trim();}";

/// W3C-AccName subset: implicit role mapping + accessible-name computation
/// (aria-label → aria-labelledby → associated label/placeholder → text → title).
const ROLE_HELPERS: &str = "function roleOf(e){var r=e.getAttribute('role');if(r)return r.trim().toLowerCase();var tag=e.tagName.toLowerCase();if(tag==='button')return 'button';if(tag==='a'&&e.hasAttribute('href'))return 'link';if(tag==='select')return 'combobox';if(tag==='textarea')return 'textbox';if(/^h[1-6]$/.test(tag))return 'heading';if(tag==='input'){var ty=(e.getAttribute('type')||'text').toLowerCase();if(['text','email','password','search','tel','url',''].indexOf(ty)!==-1)return 'textbox';if(ty==='checkbox')return 'checkbox';if(ty==='radio')return 'radio';if(ty==='button'||ty==='submit'||ty==='reset')return 'button';}return '';}function accName(e){var al=e.getAttribute('aria-label');if(al&&al.trim())return norm(al);var lb=e.getAttribute('aria-labelledby');if(lb){var txt=lb.split(/\\s+/).map(function(id){var t=document.getElementById(id);return t?t.textContent:'';}).join(' ');if(norm(txt))return norm(txt);}var tag=e.tagName.toLowerCase();if(tag==='input'||tag==='textarea'||tag==='select'){if(e.id){try{var lbl=document.querySelector('label[for=\"'+(window.CSS&&CSS.escape?CSS.escape(e.id):e.id)+'\"]');if(lbl&&norm(lbl.textContent))return norm(lbl.textContent);}catch(_e){}}var pl=e.getAttribute('placeholder');if(pl&&pl.trim())return pl.trim();}var tc=norm(e.textContent);if(tc)return tc;var ti=e.getAttribute('title');if(ti&&ti.trim())return ti.trim();return '';}";

fn wrap(body: &str) -> String {
    let mut s = String::from("(function(){");
    s.push_str(body);
    s.push_str("})()");
    s
}

/// JS resolver expression for a `text=`/`role=` segment, or `None` for others.
fn marker_resolver_js(seg: &Segment) -> Option<String> {
    match seg {
        Segment::Text(needle) => Some(text_resolver_js(needle)),
        Segment::Role(spec) => {
            let (role, name) = parse_role_spec(spec);
            Some(role_resolver_js(&role, name.as_deref()))
        }
        Segment::Css(_) | Segment::Frame(_) => None,
    }
}

/// First *visible* element whose normalized text contains `needle` (case-
/// insensitive), preferring the smallest such element (most specific match).
fn text_resolver_js(needle: &str) -> String {
    let n = serde_json::to_string(needle).unwrap_or_else(|_| "\"\"".into());
    let mut body = String::from(JS_PRELUDE);
    body.push_str("var needle=norm(");
    body.push_str(&n);
    body.push_str(").toLowerCase();if(!needle)return false;");
    body.push_str("var best=null,bestLen=1e9,all=document.querySelectorAll('body *');for(var i=0;i<all.length;i++){var e=all[i];if(!vis(e))continue;var t=norm(e.textContent).toLowerCase();if(t.indexOf(needle)!==-1&&t.length<bestLen){best=e;bestLen=t.length;}}if(best){best.setAttribute(M,'1');return true;}return false;");
    wrap(&body)
}

/// First *visible* element whose computed ARIA role equals `role` and (when
/// `name` is given) whose accessible name contains it (case-insensitive).
fn role_resolver_js(role: &str, name: Option<&str>) -> String {
    let role_j = serde_json::to_string(&role.to_lowercase()).unwrap_or_else(|_| "\"\"".into());
    let name_j = match name {
        Some(n) => serde_json::to_string(&n.to_lowercase()).unwrap_or_else(|_| "\"\"".into()),
        None => "null".into(),
    };
    let mut body = String::from(JS_PRELUDE);
    body.push_str(ROLE_HELPERS);
    body.push_str("var wantRole=");
    body.push_str(&role_j);
    body.push_str(";var wantName=");
    body.push_str(&name_j);
    body.push_str(";var best=null,bestLen=1e9,all=document.querySelectorAll('body *');for(var i=0;i<all.length;i++){var e=all[i];if(!vis(e))continue;if(roleOf(e)!==wantRole)continue;var an=norm(accName(e)).toLowerCase();if(wantName!==null&&an.indexOf(wantName)===-1)continue;if(an.length<bestLen){best=e;bestLen=an.length;}}if(best){best.setAttribute(M,'1');return true;}return false;");
    wrap(&body)
}

/// Split `role=` value `NAME[name="X"]` into `("name"…, Some("X"))`. Tolerates
/// single/double quotes and an unquoted value terminated by `]`/space.
fn parse_role_spec(spec: &str) -> (String, Option<String>) {
    match spec.find('[') {
        Some(br) => {
            let role = spec[..br].trim().to_string();
            let rest = &spec[br..];
            let name = rest.find("name=").map(|i| &rest[i + 5..]).map(|s| {
                let s = s.trim_start();
                let (quote, s) = match s.chars().next() {
                    Some(q @ ('"' | '\'')) => (Some(q), &s[1..]),
                    _ => (None, s),
                };
                let end = match quote {
                    Some(q) => s.find(q).unwrap_or(s.len()),
                    None => s.find([']', ' ']).unwrap_or(s.len()),
                };
                s[..end].to_string()
            });
            (role, name)
        }
        None => (spec.trim().to_string(), None),
    }
}

#[cfg(test)]
mod locator_resolver_tests {
    use super::*;

    #[test]
    fn parse_role_spec_extracts_role_and_quoted_name() {
        assert_eq!(
            parse_role_spec(r#"button[name="Send"]"#),
            ("button".into(), Some("Send".into()))
        );
        assert_eq!(parse_role_spec("button"), ("button".into(), None));
        assert_eq!(
            parse_role_spec("textbox[name='Email']"),
            ("textbox".into(), Some("Email".into()))
        );
    }

    #[test]
    fn text_resolver_embeds_needle_safely() {
        // A needle with a quote must not break out of the JS string literal.
        let js = text_resolver_js(r#"a"b"#);
        assert!(
            js.contains(r#""a\"b""#),
            "needle must be JSON-escaped: {js}"
        );
        assert!(js.starts_with("(function(){") && js.ends_with("})()"));
    }

    #[test]
    fn role_resolver_includes_accname_helpers() {
        let js = role_resolver_js("button", Some("Continue"));
        assert!(js.contains("function accName"));
        assert!(
            js.contains("\"continue\""),
            "name lowercased + embedded: {js}"
        );
    }
}
