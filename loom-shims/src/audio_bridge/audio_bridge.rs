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
use std::time::{Duration, Instant};

use base64::Engine as _;
use ciborium::value::Value as CborValue;
use dashmap::DashMap;

use super::resample::linear_resample;
use super::wav::{f32_to_i16, write_wav_mono_i16};
use crate::cdp_connection::cdp_connection::{CdpConnection, CdpError};
use crate::ipc_endpoint::ipc_endpoint::CdpMessage;
use crate::target_manager::target_manager::TargetManager;
use loom_shared::navigate_outcome::{AudioCaptureOutcome, AudioInjectOutcome};
use loom_shared::shim_protocol::{SessionId, TargetId};

/// Capture output frame rate (mono 16 kHz i16 — the STT sink; PRD D4).
const CAPTURE_RATE_HZ: u32 = 16_000;

/// Bytes of canonical RIFF/WAVE header prepended by `write_wav_mono_i16`. Reserved
/// from `max_bytes` so the RETURNED WAV (header + body) stays ≤ the byte cap (I2).
const WAV_HEADER_BYTES: u64 = 44;

/// Max decoded bytes pulled per `drain` call (PRD D3 bounded exfil, ≤ 1 MiB); the
/// base64 CDP string is ~4/3 of this.
const MAX_DRAIN_BYTES: u64 = 1_048_576;

/// Hard total ceiling on decoded native f32 bytes across the whole drain loop — an
/// OOM backstop above any legitimate `Caps` (PRD D3 "over-ceiling drain is a typed
/// error, not an OOM"). 64 MiB of native f32 ≈ 5.6 min @ 48 kHz.
const MAX_DRAIN_TOTAL_BYTES: usize = 64 * 1024 * 1024;

/// Wall-clock deadline for the whole stop→drain loop (PRD latency table:
/// `capture stop → ContentRef` 10 s → typed `capture_drain_timeout`).
const DRAIN_DEADLINE_MS: u64 = 10_000;

/// Per-call CDP timeout for the capture control/drain commands.
const CAPTURE_CMD_TIMEOUT_MS: u64 = 5_000;

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

/// Resource caps for one capture. `0` (or absurd) maps to a safe default, so the
/// public `web.start_audio_capture` API can never DISABLE a cap by passing 0
/// (mirrors `screencast_recorder::Caps::sanitized`). Truncation, not error (D3).
#[derive(Debug, Clone, Copy)]
pub struct Caps {
    pub max_duration_ms: u64,
    pub max_bytes: u64,
}

impl Default for Caps {
    fn default() -> Self {
        Caps {
            max_duration_ms: 300_000,    // 5 min
            max_bytes: 16 * 1024 * 1024, // 16 MiB (matches MAX_FRAME_BYTES; ~8.7 min @ 16 kHz)
        }
    }
}

impl Caps {
    /// Build from concrete wire values, mapping `0` to the safe default. This is
    /// the only constructor the dispatcher/daemon use, so a caller cannot disable
    /// the safety caps through the public API.
    pub fn sanitized(max_duration_ms: u64, max_bytes: u64) -> Self {
        let d = Caps::default();
        Caps {
            max_duration_ms: if max_duration_ms == 0 {
                d.max_duration_ms
            } else {
                max_duration_ms
            },
            max_bytes: if max_bytes == 0 {
                d.max_bytes
            } else {
                max_bytes
            },
        }
    }
}

/// Monotonic millisecond clock, injected so tests can drive the drain-loop
/// deadline deterministically (Architecture §5 — "drain cadence only"). The
/// duration CAP itself is computed from `sample_count`, never this clock (A6).
pub trait AudioClock: Send + Sync {
    fn elapsed_ms(&self) -> u64;
}

/// Real clock: milliseconds since the bridge was constructed.
pub struct SystemClock {
    start: Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl AudioClock for SystemClock {
    fn elapsed_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
}

/// Per-target capture state. The samples live in the page's ring buffer until
/// `stop_capture` drains them; the shim only needs the caps to enforce at stop.
struct CaptureState {
    caps: Caps,
}

/// Env kill-switch: `LOOM_DISABLE_AUDIO=1` refuses capture/inject without a
/// recompile (parity with `LOOM_DISABLE_RECORDING`).
fn audio_disabled() -> bool {
    matches!(std::env::var("LOOM_DISABLE_AUDIO"), Ok(v) if v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Injects caller-provided audio into a target's synthetic microphone and (task
/// 05) captures the inbound WebRTC audio. Holds the same `cdp` handle as the rest
/// of the executor plus a `TargetManager` handle to read the per-target
/// `audio_nonce` (the address of the in-page API).
pub struct AudioBridge {
    cdp: Arc<dyn CdpConnection>,
    targets: Arc<dyn TargetManager>,
    /// Active captures keyed by REAL target id (never the `0` sentinel).
    active: DashMap<TargetId, CaptureState>,
    clock: Arc<dyn AudioClock>,
}

impl AudioBridge {
    pub fn new(cdp: Arc<dyn CdpConnection>, targets: Arc<dyn TargetManager>) -> Self {
        Self::with_clock(cdp, targets, Arc::new(SystemClock::default()))
    }

    /// Construct with an injected clock (tests drive the drain-loop deadline).
    pub fn with_clock(
        cdp: Arc<dyn CdpConnection>,
        targets: Arc<dyn TargetManager>,
        clock: Arc<dyn AudioClock>,
    ) -> Self {
        Self {
            cdp,
            targets,
            active: DashMap::new(),
            clock,
        }
    }

    /// True if a capture is active for the session's target.
    pub fn is_capturing(&self, session_id: SessionId, target_id: TargetId) -> bool {
        let target_id = self
            .targets
            .target_for_session(session_id)
            .unwrap_or(target_id);
        self.active.contains_key(&target_id)
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

    // ── task 05: capture (start/stop) ────────────────────────────────────────

    /// Open a capture window on the session's target: reserve the per-target slot
    /// atomically, then tell the in-page tap to start buffering inbound audio.
    /// Errors (no session abort): kill-switch set, non-`--audio` session, a
    /// capture already active, or the in-page `startCapture` call failing. Typed
    /// kinds mirror inject: `audio_not_enabled`, `audio_bridge_unavailable`,
    /// `capture_already_active`, `capture_start_failed`.
    pub async fn start_capture(
        &self,
        session_id: SessionId,
        target_id: TargetId,
        caps: Caps,
    ) -> Result<(), String> {
        if audio_disabled() {
            return Err("audio_not_enabled: capture disabled via LOOM_DISABLE_AUDIO".to_string());
        }
        // Host passes the `0` sentinel; resolve the real target (nonce lives there).
        let target_id = self
            .targets
            .target_for_session(session_id)
            .unwrap_or(target_id);
        let nonce = self
            .targets
            .target_state(target_id)
            .and_then(|s| s.audio_nonce)
            .ok_or_else(|| "audio_not_enabled: session was not created with --audio".to_string())?;

        // Reserve the slot BEFORE any await (TOCTOU close — two concurrent
        // start_capture()s for the same target cannot both pass; mirrors
        // `screencast_recorder.rs:275`).
        match self.active.entry(target_id) {
            dashmap::mapref::entry::Entry::Occupied(_) => {
                return Err(
                    "capture_already_active: a capture is already active for this target"
                        .to_string(),
                );
            }
            dashmap::mapref::entry::Entry::Vacant(slot) => {
                slot.insert(CaptureState { caps });
            }
        }

        let timeout = Duration::from_millis(CAPTURE_CMD_TIMEOUT_MS);
        let object_id = match self.resolve_api_object_id(target_id, &nonce, timeout).await {
            Ok(id) => id,
            Err(e) => {
                self.active.remove(&target_id); // undo the reservation
                return Err(e);
            }
        };

        // Call `api.startCapture()`; returnByValue so a page-thrown error surfaces
        // as exceptionDetails rather than a dangling object.
        let call = call_api_fn(
            &object_id,
            "function(){ return this.startCapture(); }",
            vec![],
        );
        let result = self.cdp.command(target_id, call, Some(timeout)).await;
        self.release_object(target_id, &object_id).await;

        match result {
            Ok(res) => {
                if let Some(text) = exception_text(&res) {
                    self.active.remove(&target_id);
                    return Err(format!("capture_start_failed: {text}"));
                }
                tracing::info!(target_id, "audio.capture_started");
                Ok(())
            }
            Err(e) => {
                self.active.remove(&target_id);
                Err(map_capture_cdp_error(e))
            }
        }
    }

    /// Stop the active capture, drain the in-page ring in bounded chunks, resample
    /// native → 16 kHz mono i16, enforce the caps (truncation, not error), and WAV-
    /// mux. Never errors at the call boundary (mirrors `ScreencastRecorder::stop`):
    /// stop-before-start, an empty capture, or a drain failure are reported via
    /// `stop_reason` + `error` so the daemon can emit an error receipt without
    /// aborting the session.
    pub async fn stop_capture(
        &self,
        session_id: SessionId,
        target_id: TargetId,
    ) -> AudioCaptureOutcome {
        let target_id = self
            .targets
            .target_for_session(session_id)
            .unwrap_or(target_id);

        // Stop-before-start / double-stop is a DISTINCT typed error (I2), separate
        // from a capture that ran but yielded nothing (no_samples/no_inbound_track).
        let Some((_, state)) = self.active.remove(&target_id) else {
            return AudioCaptureOutcome {
                stop_reason: "error".to_string(),
                error: Some("no active capture for this target".to_string()),
                ..Default::default()
            };
        };
        let caps = state.caps;

        let nonce = match self
            .targets
            .target_state(target_id)
            .and_then(|s| s.audio_nonce)
        {
            Some(n) => n,
            None => {
                return capture_error("audio_not_enabled: session was not created with --audio");
            }
        };

        let timeout = Duration::from_millis(CAPTURE_CMD_TIMEOUT_MS);
        let object_id = match self.resolve_api_object_id(target_id, &nonce, timeout).await {
            Ok(id) => id,
            Err(e) => return capture_error(&e),
        };

        // Close the in-page capture window (best-effort — draining still works if
        // this fails, it just means late frames may be included).
        let stop_call = call_api_fn(
            &object_id,
            "function(){ return this.stopCapture(); }",
            vec![],
        );
        let _ = self.cdp.command(target_id, stop_call, Some(timeout)).await;

        // Drain loop: pull ≤ MAX_DRAIN_BYTES decoded per call until `more == false`,
        // bounded by a hard total ceiling and a wall-clock deadline.
        let mut native: Vec<f32> = Vec::new();
        let mut source_rate: u32 = 0;
        let mut dropped_frames: u64 = 0;
        let mut tapped_tracks: u64 = 0;
        let mut injected_leaked = false;
        let mut buffer_cap_hit = false;
        let mut drain_error: Option<String> = None;

        let started_ms = self.clock.elapsed_ms();
        loop {
            if self.clock.elapsed_ms().saturating_sub(started_ms) > DRAIN_DEADLINE_MS {
                drain_error = Some("capture_drain_timeout".to_string());
                break;
            }
            let drain_call = call_api_fn(
                &object_id,
                "function(n){ return this.drain(n); }",
                vec![CborValue::Integer((MAX_DRAIN_BYTES as i64).into())],
            );
            let res = match self.cdp.command(target_id, drain_call, Some(timeout)).await {
                Ok(r) => r,
                Err(e) => {
                    drain_error = Some(map_capture_cdp_error(e));
                    break;
                }
            };
            if let Some(text) = exception_text(&res) {
                drain_error = Some(format!("capture_drain_failed: {text}"));
                break;
            }
            let Some(d) = parse_drain(&res) else {
                drain_error = Some("capture_drain_failed: malformed drain payload".to_string());
                break;
            };
            if d.sample_rate > 0 {
                source_rate = d.sample_rate;
            }
            // The page reports these cumulatively; `max` is also robust if a future
            // page ever reported per-call (never undercount / never re-zero — M2).
            dropped_frames = dropped_frames.max(d.dropped_frames);
            tapped_tracks = tapped_tracks.max(d.tapped_tracks);
            injected_leaked |= d.injected_leaked;
            buffer_cap_hit |= d.buffer_cap_hit;

            if !d.samples_b64.is_empty() {
                // M3: bound a single response BEFORE decoding so a hostile/buggy page
                // can't spike memory past the per-call budget in one oversized drain
                // (`MAX_DRAIN_BYTES` is only a hint the page may ignore). base64 of N
                // bytes is ~4N/3, so 2× the byte budget is generous headroom.
                if d.samples_b64.len() as u64 > MAX_DRAIN_BYTES * 2 {
                    drain_error = Some("capture_drain_overflow".to_string());
                    break;
                }
                match base64::engine::general_purpose::STANDARD.decode(d.samples_b64.as_bytes()) {
                    Ok(raw) => {
                        // M4: each payload must be a whole number of little-endian f32s;
                        // a misaligned length would silently drop bytes under chunks_exact.
                        if raw.len() % 4 != 0 {
                            drain_error =
                                Some("capture_drain_failed: misaligned sample payload".to_string());
                            break;
                        }
                        for chunk in raw.as_chunks::<4>().0 {
                            native.push(f32::from_le_bytes(*chunk));
                        }
                    }
                    Err(e) => {
                        drain_error = Some(format!("capture_drain_failed: base64 {e}"));
                        break;
                    }
                }
            }
            if native.len() * 4 > MAX_DRAIN_TOTAL_BYTES {
                drain_error = Some("capture_drain_overflow".to_string());
                break;
            }
            if !d.more {
                break;
            }
        }

        self.release_object(target_id, &object_id).await;

        self.build_capture_outcome(
            target_id,
            caps,
            native,
            source_rate,
            dropped_frames,
            tapped_tracks,
            injected_leaked,
            buffer_cap_hit,
            drain_error,
        )
    }

    /// Turn the drained native f32 samples into the final `AudioCaptureOutcome`:
    /// resample → i16 → cap-enforce (truncation) → WAV mux, or report the
    /// appropriate typed `stop_reason` for a failure / empty capture.
    #[allow(clippy::too_many_arguments)]
    fn build_capture_outcome(
        &self,
        target_id: TargetId,
        caps: Caps,
        native: Vec<f32>,
        source_rate: u32,
        dropped_frames: u64,
        tapped_tracks: u64,
        injected_leaked: bool,
        buffer_cap_hit: bool,
        drain_error: Option<String>,
    ) -> AudioCaptureOutcome {
        // A2 tripwire: our own outbound audio must never appear in the capture.
        if injected_leaked {
            tracing::error!(
                target_id,
                "audio.capture injected track leaked into capture (A2)"
            );
            return AudioCaptureOutcome {
                stop_reason: "error".to_string(),
                error: Some("injected_track_leaked".to_string()),
                dropped_frames,
                source_sample_rate: source_rate,
                ..Default::default()
            };
        }
        if let Some(err) = drain_error {
            tracing::warn!(target_id, error = %err, "audio.capture drain failed");
            return AudioCaptureOutcome {
                stop_reason: "error".to_string(),
                error: Some(err),
                dropped_frames,
                source_sample_rate: source_rate,
                ..Default::default()
            };
        }
        if native.is_empty() || source_rate == 0 {
            // I1: distinguish "WebRTC never connected" from "connected but silent".
            let reason = if tapped_tracks == 0 {
                "no_inbound_track"
            } else {
                "no_samples"
            };
            return AudioCaptureOutcome {
                stop_reason: reason.to_string(),
                error: Some(format!("captured zero samples ({reason})")),
                dropped_frames,
                source_sample_rate: source_rate,
                ..Default::default()
            };
        }

        // Resample native → 16 kHz, then f32 → i16.
        let resampled = linear_resample(&native, source_rate, CAPTURE_RATE_HZ);
        let mut samples: Vec<i16> = resampled.iter().map(|&s| f32_to_i16(s)).collect();

        // Enforce caps by truncation (never error). BOTH caps are evaluated and the
        // TIGHTER one wins (I1 — they must combine, not short-circuit on the first).
        // A `0` cap means "unlimited" (parity with `Caps::sanitized`), so even a raw
        // `Caps` literal cannot silently drop all audio (M1). The byte cap reserves
        // the WAV header so the returned artifact stays ≤ `max_bytes` (I2).
        let mut stop_reason = "explicit".to_string();
        let mut limit = samples.len();
        if caps.max_bytes > 0 {
            let body_bytes = caps.max_bytes.saturating_sub(WAV_HEADER_BYTES);
            let by_bytes = (body_bytes / 2) as usize; // 2 bytes per i16 sample
            if by_bytes < limit {
                limit = by_bytes;
                stop_reason = "byte_cap".to_string();
            }
        }
        if caps.max_duration_ms > 0 {
            let by_dur = (caps
                .max_duration_ms
                .saturating_mul(u64::from(CAPTURE_RATE_HZ))
                / 1000) as usize;
            if by_dur < limit {
                limit = by_dur;
                stop_reason = "duration_cap".to_string();
            }
        }
        if limit < samples.len() {
            samples.truncate(limit);
        } else if buffer_cap_hit {
            // The in-page ring hit its hard ceiling before any caller cap — still a
            // truncation the caller must be told about (D3, C2).
            stop_reason = "byte_cap".to_string();
        }
        if stop_reason == "byte_cap" || stop_reason == "duration_cap" {
            // D3 / FND-0006: silent truncation is a trust failure — loud WARN.
            tracing::warn!(
                target_id,
                stop_reason = %stop_reason,
                sample_count = samples.len(),
                "audio.capture truncated to cap"
            );
        }

        let sample_count = samples.len() as u64;
        let duration_ms = sample_count.saturating_mul(1000) / u64::from(CAPTURE_RATE_HZ);
        let wav_bytes = write_wav_mono_i16(&samples, CAPTURE_RATE_HZ);

        tracing::info!(
            target_id,
            stop_reason = %stop_reason,
            sample_count,
            dropped_frames,
            source_sample_rate = source_rate,
            "audio.capture_stopped"
        );

        AudioCaptureOutcome {
            wav_bytes,
            sample_count,
            duration_ms,
            dropped_frames,
            source_sample_rate: source_rate,
            stop_reason,
            error: None,
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

/// Map a transport-level `CdpError` to a typed CAPTURE error string.
fn map_capture_cdp_error(e: CdpError) -> String {
    match e {
        CdpError::Timeout { .. } => {
            "capture_drain_timeout: capture exceeded its deadline".to_string()
        }
        other => format!("capture_start_failed: {other}"),
    }
}

/// Build a `Runtime.callFunctionOn` message for the nonce'd in-page api object.
/// `func_decl` is a fixed, self-authored function string (never caller data);
/// `args` are passed as `CallArgument`s (A3 — never interpolated into JS).
fn call_api_fn(object_id: &str, func_decl: &str, args: Vec<CborValue>) -> CdpMessage {
    let arg_values: Vec<CborValue> = args
        .into_iter()
        .map(|v| CborValue::Map(vec![(CborValue::Text("value".into()), v)]))
        .collect();
    CdpMessage {
        method: "Runtime.callFunctionOn".into(),
        params: CborValue::Map(vec![
            (
                CborValue::Text("functionDeclaration".into()),
                CborValue::Text(func_decl.to_string()),
            ),
            (
                CborValue::Text("objectId".into()),
                CborValue::Text(object_id.to_string()),
            ),
            (
                CborValue::Text("arguments".into()),
                CborValue::Array(arg_values),
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
    }
}

/// A parsed in-page `drain()` result.
struct DrainResult {
    samples_b64: String,
    sample_rate: u32,
    dropped_frames: u64,
    tapped_tracks: u64,
    injected_leaked: bool,
    buffer_cap_hit: bool,
    more: bool,
}

/// Parse the `result.value` object of a `callFunctionOn(drain)` response.
fn parse_drain(v: &CborValue) -> Option<DrainResult> {
    let value = cbor_get(cbor_get(v, "result")?, "value")?;
    Some(DrainResult {
        samples_b64: cbor_str(value, "samples_b64").unwrap_or_default(),
        sample_rate: cbor_u64(value, "sample_rate").unwrap_or(0) as u32,
        dropped_frames: cbor_u64(value, "dropped_frames").unwrap_or(0),
        tapped_tracks: cbor_u64(value, "tapped_tracks").unwrap_or(0),
        injected_leaked: cbor_bool(value, "injected_leaked").unwrap_or(false),
        buffer_cap_hit: cbor_bool(value, "buffer_cap_hit").unwrap_or(false),
        more: cbor_bool(value, "more").unwrap_or(false),
    })
}

/// A best-effort capture failure outcome (empty WAV + typed `error`, stop_reason
/// `error`) so `stop_capture` never errors at the call boundary.
fn capture_error(msg: &str) -> AudioCaptureOutcome {
    AudioCaptureOutcome {
        stop_reason: "error".to_string(),
        error: Some(msg.to_string()),
        ..Default::default()
    }
}

/// Read a `Text` field from a CBOR map.
fn cbor_str(v: &CborValue, key: &str) -> Option<String> {
    match cbor_get(v, key)? {
        CborValue::Text(s) => Some(s.clone()),
        _ => None,
    }
}

/// Read an integer field (non-negative) from a CBOR map.
fn cbor_u64(v: &CborValue, key: &str) -> Option<u64> {
    match cbor_get(v, key)? {
        CborValue::Integer(i) => u64::try_from(i128::from(*i)).ok(),
        CborValue::Float(f) if *f >= 0.0 => Some(*f as u64),
        _ => None,
    }
}

/// Read a bool field from a CBOR map.
fn cbor_bool(v: &CborValue, key: &str) -> Option<bool> {
    match cbor_get(v, key)? {
        CborValue::Bool(b) => Some(*b),
        _ => None,
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
