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
use super::process::send_and_await;
use super::shim_manager::ShimManager;
use super::types::{
    EvaluateOutcome, FailureClass, SendEvaluateParams, SendNavigateParams, SendSetInputFilesParams,
    SendWaitForParams, SetInputFilesOutcome, ShimId,
};
use loom_core::error::{LoomError, LoomErrorCode};
use loom_shared::navigate_outcome::{NavigateOutcome, NetworkLogOutcome, ScreencastOutcome};
use loom_shared::shim_protocol::{CdpMessage, ShimRequest, ShimResponse};
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
}
