// audio_bridge::inject — voice-call-io task 04.
//
// Hands daemon-resolved audio bytes to the per-session nonce'd in-page enqueue
// hook installed by task 03 (`window.__loom_<nonce>.enqueue`). The bytes travel
// as a `Runtime.callFunctionOn` `CallArgument`, NEVER string-interpolated into JS
// source (Architecture A3): only the self-generated nonce is baked into the
// function/expression text.
//
// The shim stays CAS-free: the daemon (`wasm_bridge`) has already resolved the
// blob-ref / inline-base64 payload to raw bytes and size-bounded it (≤ 8 MiB).
//
// Task 05 (capture) extends this same struct with `start_capture`/`stop_capture`
// + a per-target `CaptureState` map; task 04 keeps it minimal (stateless inject).

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use ciborium::value::Value as CborValue;

use crate::cdp_connection::cdp_connection::{CdpConnection, CdpError};
use crate::ipc_endpoint::ipc_endpoint::CdpMessage;
use crate::target_manager::target_manager::TargetManager;
use loom_shared::navigate_outcome::AudioInjectOutcome;
use loom_shared::shim_protocol::{SessionId, TargetId};

/// Bound on the enqueue-only dispatch (`await_playout: false`, the default). The
/// clip is scheduled and we return immediately, so this only needs to cover the
/// CDP round-trip + `decodeAudioData` (PRD D13 targets 200 ms p95; the timeout is
/// a generous hard kill, not the SLA).
const INJECT_DISPATCH_TIMEOUT_MS: u64 = 5_000;

/// Hard ceiling on `await_playout: true`. Without this a long clip (up to 8 MiB
/// decoded) would hold the calling thread for the whole playout — the exact
/// thread-exhaustion the plan council flagged (C1). On expiry the CDP call is
/// killed and the caller gets a typed `inject_timeout`; the session stays usable.
const AWAIT_PLAYOUT_CEILING_MS: u64 = 60_000;

/// Injects caller-provided audio into a target's synthetic microphone. Holds the
/// same `cdp` handle as the rest of the executor plus a `TargetManager` handle to
/// read the per-target `audio_nonce` (the address of the in-page API).
pub struct AudioBridge {
    cdp: Arc<dyn CdpConnection>,
    targets: Arc<dyn TargetManager>,
}

impl AudioBridge {
    pub fn new(cdp: Arc<dyn CdpConnection>, targets: Arc<dyn TargetManager>) -> Self {
        Self { cdp, targets }
    }

    /// Enqueue `bytes` on the target's synthetic mic. Returns an observational
    /// [`AudioInjectOutcome`] (duration + whether playout was awaited) on success;
    /// an `Err(typed_kind)` string on failure (the dispatcher maps it to a
    /// `ShimResponse::Error`). Typed kinds: `audio_not_enabled`,
    /// `no_microphone_request`, `audio_decode_failed`, `inject_timeout`,
    /// `audio_bridge_unavailable`, `inject_failed`.
    pub async fn inject(
        &self,
        session_id: SessionId,
        target_id: TargetId,
        bytes: &[u8],
        await_playout: bool,
    ) -> Result<AudioInjectOutcome, String> {
        // The host passes `target_id: 0` (the "session's active target" sentinel,
        // like network_log/recording). The audio nonce lives on the REAL target's
        // state, so resolve it via the session binding rather than keying on 0.
        let target_id = self
            .targets
            .target_for_session(session_id)
            .unwrap_or(target_id);

        // The in-page API lives at `window.__loom_<nonce>`; the nonce was minted
        // and stored on the target when the `--audio` bootstrap was installed
        // (task 03). No nonce ⇒ this session was not created with `--audio`.
        let nonce = self
            .targets
            .target_state(target_id)
            .and_then(|s| s.audio_nonce)
            .ok_or_else(|| "audio_not_enabled: session was not created with --audio".to_string())?;

        // A3: bytes are base64-encoded and passed as a CallArgument. Only the
        // nonce (a self-generated hex string) is interpolated into JS text.
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);

        let timeout = Duration::from_millis(if await_playout {
            AWAIT_PLAYOUT_CEILING_MS
        } else {
            INJECT_DISPATCH_TIMEOUT_MS
        });

        // Step 1: resolve the objectId of the in-page API object. `callFunctionOn`
        // requires an objectId or executionContextId, and the shim tracks neither,
        // so we resolve it per-call. A stale objectId (mid-inject navigation)
        // surfaces here as a typed error, never a hang (the call is timeout-bound).
        let object_id = self
            .resolve_api_object_id(target_id, &nonce, timeout)
            .await?;

        // Step 2: call `api.enqueue(b64, await_playout)` and await the Promise so
        // enqueue-rejections (no_microphone_request / audio_decode_failed) surface
        // as `exceptionDetails`, and `await_playout` blocks until `onended`.
        let call = CdpMessage {
            method: "Runtime.callFunctionOn".into(),
            params: CborValue::Map(vec![
                (
                    CborValue::Text("functionDeclaration".into()),
                    CborValue::Text("function(b64, ap){ return this.enqueue(b64, ap); }".into()),
                ),
                (
                    CborValue::Text("objectId".into()),
                    CborValue::Text(object_id.clone()),
                ),
                (
                    CborValue::Text("arguments".into()),
                    CborValue::Array(vec![
                        CborValue::Map(vec![(
                            CborValue::Text("value".into()),
                            CborValue::Text(b64),
                        )]),
                        CborValue::Map(vec![(
                            CborValue::Text("value".into()),
                            CborValue::Bool(await_playout),
                        )]),
                    ]),
                ),
                (
                    CborValue::Text("returnByValue".into()),
                    CborValue::Bool(true),
                ),
                (
                    CborValue::Text("awaitPromise".into()),
                    CborValue::Bool(true),
                ),
            ]),
        };

        let call_result = self.cdp.command(target_id, call, Some(timeout)).await;

        // Release the renderer-side object handle regardless of the call outcome —
        // `Runtime.evaluate {returnByValue:false}` retains it until released, so a
        // long-lived session doing repeated injects (AC2) would otherwise leak one
        // objectId per call. Best-effort: a release failure is only logged.
        self.release_object(target_id, &object_id).await;

        let result = call_result.map_err(map_cdp_error)?;

        // A rejected enqueue Promise comes back as `exceptionDetails`; map the
        // known page-thrown errors, log + surface anything unexpected.
        if let Some(text) = exception_text(&result) {
            return Err(map_enqueue_exception(&text));
        }

        // `result.value` is the clip duration in seconds (see audio_bootstrap.js
        // `enqueue` → resolves `buf.duration`). Absence is non-fatal (older
        // bootstrap / fake double) → duration 0.
        let duration_ms = eval_result_number(&result)
            .map(|secs| (secs * 1000.0).round().max(0.0) as u64)
            .unwrap_or(0);

        // D18 observability: one structured event per successful inject.
        tracing::info!(
            target_id,
            duration_ms,
            awaited_playout = await_playout,
            "audio.inject_enqueued"
        );

        Ok(AudioInjectOutcome {
            duration_ms,
            awaited_playout: await_playout,
        })
    }

    /// `Runtime.evaluate window.__loom_<nonce>` → the api object's `objectId`.
    async fn resolve_api_object_id(
        &self,
        target_id: TargetId,
        nonce: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        let eval = CdpMessage {
            method: "Runtime.evaluate".into(),
            params: CborValue::Map(vec![
                (
                    CborValue::Text("expression".into()),
                    // Nonce is a self-generated hex string — safe to interpolate.
                    CborValue::Text(format!("window.__loom_{nonce}")),
                ),
                (
                    CborValue::Text("returnByValue".into()),
                    CborValue::Bool(false),
                ),
            ]),
        };
        let res = self
            .cdp
            .command(target_id, eval, Some(timeout))
            .await
            .map_err(map_cdp_error)?;

        eval_result_object_id(&res).ok_or_else(|| {
            // The nonce'd global is absent — the bootstrap did not install (or the
            // page wiped it). Typed + retryable.
            "audio_bridge_unavailable: in-page audio API not found (window.__loom_<nonce>)"
                .to_string()
        })
    }

    /// Best-effort `Runtime.releaseObject` for the resolved api-object handle so
    /// repeated injects don't leak renderer-side objects. A failure is only logged
    /// (the object is released anyway when the execution context is torn down).
    async fn release_object(&self, target_id: TargetId, object_id: &str) {
        let msg = CdpMessage {
            method: "Runtime.releaseObject".into(),
            params: CborValue::Map(vec![(
                CborValue::Text("objectId".into()),
                CborValue::Text(object_id.to_string()),
            )]),
        };
        if let Err(e) = self
            .cdp
            .command(
                target_id,
                msg,
                Some(Duration::from_millis(INJECT_DISPATCH_TIMEOUT_MS)),
            )
            .await
        {
            tracing::debug!(target_id, error = %e, "audio.inject releaseObject failed (non-fatal)");
        }
    }
}

/// Map a transport-level `CdpError` to a typed inject error string.
fn map_cdp_error(e: CdpError) -> String {
    match e {
        CdpError::Timeout { .. } => {
            "inject_timeout: audio inject exceeded its deadline".to_string()
        }
        other => format!("inject_failed: {other}"),
    }
}

/// Map a page-thrown enqueue rejection (from CDP `exceptionDetails`) to a typed kind.
fn map_enqueue_exception(text: &str) -> String {
    if text.contains("no_microphone_request") {
        "no_microphone_request".to_string()
    } else if text.contains("audio_decode_failed") || text.to_lowercase().contains("decode") {
        "audio_decode_failed".to_string()
    } else {
        // Unmapped page exception — surface loudly (plan council: observability).
        tracing::warn!(exception = %text, "audio.inject unmapped CDP exception");
        format!("inject_failed: {text}")
    }
}

/// Pull `result.objectId` from a `Runtime.evaluate` response.
fn eval_result_object_id(v: &CborValue) -> Option<String> {
    match cbor_get(cbor_get(v, "result")?, "objectId")? {
        CborValue::Text(s) => Some(s.clone()),
        _ => None,
    }
}

/// Pull `result.value` (a number) from a `Runtime.evaluate`/`callFunctionOn` response.
fn eval_result_number(v: &CborValue) -> Option<f64> {
    let value = cbor_get(cbor_get(v, "result")?, "value")?;
    match value {
        CborValue::Float(f) => Some(*f),
        CborValue::Integer(i) => Some(i128::from(*i) as f64),
        _ => None,
    }
}

/// Extract a human-readable message from a CDP `exceptionDetails` block, if present.
/// Prefers `exception.description`, falls back to `text`.
fn exception_text(v: &CborValue) -> Option<String> {
    let details = cbor_get(v, "exceptionDetails")?;
    if let Some(CborValue::Text(desc)) =
        cbor_get(details, "exception").and_then(|e| cbor_get(e, "description"))
    {
        return Some(desc.clone());
    }
    match cbor_get(details, "text") {
        Some(CborValue::Text(t)) => Some(t.clone()),
        _ => Some("unknown CDP exception".to_string()),
    }
}

/// Local CBOR map lookup (mirrors `settle_driver::cbor_get`).
fn cbor_get<'a>(v: &'a CborValue, key: &str) -> Option<&'a CborValue> {
    if let CborValue::Map(entries) = v {
        for (k, val) in entries {
            if let CborValue::Text(k) = k {
                if k == key {
                    return Some(val);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_object_id_from_evaluate() {
        let res = CborValue::Map(vec![(
            CborValue::Text("result".into()),
            CborValue::Map(vec![
                (
                    CborValue::Text("type".into()),
                    CborValue::Text("object".into()),
                ),
                (
                    CborValue::Text("objectId".into()),
                    CborValue::Text("{\"injectedScriptId\":1,\"id\":7}".into()),
                ),
            ]),
        )]);
        assert_eq!(
            eval_result_object_id(&res).as_deref(),
            Some("{\"injectedScriptId\":1,\"id\":7}")
        );
    }

    #[test]
    fn extracts_number_result() {
        let res = CborValue::Map(vec![(
            CborValue::Text("result".into()),
            CborValue::Map(vec![
                (
                    CborValue::Text("type".into()),
                    CborValue::Text("number".into()),
                ),
                (CborValue::Text("value".into()), CborValue::Float(0.8)),
            ]),
        )]);
        assert_eq!(eval_result_number(&res), Some(0.8));
    }

    #[test]
    fn maps_no_microphone_request_exception() {
        assert_eq!(
            map_enqueue_exception("Error: no_microphone_request\n    at <anonymous>"),
            "no_microphone_request"
        );
    }

    #[test]
    fn maps_decode_failure_exception() {
        assert_eq!(
            map_enqueue_exception("EncodingError: Unable to decode audio data"),
            "audio_decode_failed"
        );
        assert_eq!(
            map_enqueue_exception("Error: audio_decode_failed"),
            "audio_decode_failed"
        );
    }

    #[test]
    fn unmapped_exception_is_inject_failed() {
        let out = map_enqueue_exception("TypeError: something else");
        assert!(out.starts_with("inject_failed:"), "got {out}");
    }

    #[test]
    fn timeout_maps_to_inject_timeout() {
        assert!(map_cdp_error(CdpError::Timeout { ms: 5000 }).starts_with("inject_timeout"));
    }

    #[test]
    fn exception_text_prefers_description() {
        let res = CborValue::Map(vec![(
            CborValue::Text("exceptionDetails".into()),
            CborValue::Map(vec![(
                CborValue::Text("exception".into()),
                CborValue::Map(vec![(
                    CborValue::Text("description".into()),
                    CborValue::Text("Error: no_microphone_request".into()),
                )]),
            )]),
        )]);
        assert_eq!(
            exception_text(&res).as_deref(),
            Some("Error: no_microphone_request")
        );
    }
}
