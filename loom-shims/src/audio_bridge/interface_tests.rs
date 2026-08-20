//! `AudioBridge::inject` tests — drive the inject flow against a `TestCdp` +
//! `FakeTargets` double (no real Chromium). These prove the CDP MECHANICS:
//! that inject resolves the api objectId then calls `enqueue` with the audio
//! bytes as a **CallArgument** (never string-interpolated, Architecture A3),
//! threads `await_playout`, and maps page rejections to typed errors. The
//! audio-actually-reaches-the-track proof is the `#[ignore]`d real-browser
//! check in `loom-cli/tests/live_voice_e2e.rs`; the full round-trip is task 07.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use ciborium::value::Value as CborValue;
use parking_lot::Mutex;

use super::audio_bridge::{AudioClock, Caps};
use super::AudioBridge;
use crate::cdp_connection::{
    CdpConnection, CdpError, EventFilter, EventHandler, EventRegistration,
};
use crate::target_manager::target_manager::{TargetError, TargetManager, TargetState};
use loom_shared::shim_protocol::{CdpMessage, SessionId, TargetId};
use loom_shared::types::{EpochMs, Seed};

const TARGET: TargetId = 7;
const SESSION: SessionId = 1;
/// The host always passes target_id 0 (the "session's active target" sentinel);
/// inject must resolve the real target via target_for_session(session_id).
const TARGET_SENTINEL: TargetId = 0;
const NONCE: &str = "deadbeefcafe0001";

/// CDP double: records every issued command (full message, for arg assertions)
/// and returns canned results — `{result:{objectId}}` for `Runtime.evaluate`,
/// a configurable value/exception for `Runtime.callFunctionOn`.
#[derive(Clone)]
struct TestCdp {
    commands: Arc<Mutex<Vec<CdpMessage>>>,
    /// What `Runtime.callFunctionOn` returns for the inject `enqueue` call
    /// (default: `{result:{value:0.8}}`).
    callfn_response: Arc<Mutex<CborValue>>,
    /// FIFO of scripted `drain()` responses (task 05 capture). Each
    /// `callFunctionOn(this.drain(n))` pops one; when empty, an empty
    /// `more:false` drain is returned (buffer exhausted).
    drain_queue: Arc<Mutex<VecDeque<CborValue>>>,
    /// When set, `Runtime.evaluate` returns no objectId (api global absent).
    evaluate_missing: Arc<Mutex<bool>>,
    /// Optional forced transport error on the next command.
    force_error: Arc<Mutex<Option<CdpError>>>,
}

impl Default for TestCdp {
    fn default() -> Self {
        Self {
            commands: Arc::new(Mutex::new(Vec::new())),
            callfn_response: Arc::new(Mutex::new(number_result(0.8))),
            drain_queue: Arc::new(Mutex::new(VecDeque::new())),
            evaluate_missing: Arc::new(Mutex::new(false)),
            force_error: Arc::new(Mutex::new(None)),
        }
    }
}

#[async_trait]
impl CdpConnection for TestCdp {
    async fn connect(&self, _ws_url: &str) -> Result<(), CdpError> {
        Ok(())
    }
    async fn command(
        &self,
        _target_id: TargetId,
        msg: CdpMessage,
        _timeout: Option<Duration>,
    ) -> Result<CborValue, CdpError> {
        if let Some(e) = self.force_error.lock().take() {
            return Err(e);
        }
        let method = msg.method.clone();
        let decl = func_declaration(&msg.params).unwrap_or_default();
        self.commands.lock().push(msg);
        match method.as_str() {
            "Runtime.evaluate" => {
                if *self.evaluate_missing.lock() {
                    Ok(CborValue::Map(vec![(
                        CborValue::Text("result".into()),
                        CborValue::Map(vec![(
                            CborValue::Text("type".into()),
                            CborValue::Text("undefined".into()),
                        )]),
                    )]))
                } else {
                    Ok(object_id_result("OBJ1"))
                }
            }
            "Runtime.callFunctionOn" => {
                // Route by the in-page fn the shim invoked (task 05 capture vs
                // task 04 inject) so one double serves both.
                if decl.contains("startCapture") || decl.contains("stopCapture") {
                    Ok(ok_true_result())
                } else if decl.contains("drain") {
                    let popped = self.drain_queue.lock().pop_front();
                    Ok(popped.unwrap_or_else(|| drain_result(&[], 0, 0, 1, false, false, false)))
                } else {
                    // inject `enqueue`
                    Ok(self.callfn_response.lock().clone())
                }
            }
            "Runtime.releaseObject" => Ok(CborValue::Null),
            other => panic!("unexpected CDP method issued: {other}"),
        }
    }
    fn register_event_handler(
        &self,
        _filter: EventFilter,
        _handler: EventHandler,
    ) -> EventRegistration {
        EventRegistration::detached(0)
    }
    fn invalidate_session(&self) {}
    fn is_connected(&self) -> bool {
        true
    }
}

impl TestCdp {
    fn methods(&self) -> Vec<String> {
        self.commands
            .lock()
            .iter()
            .map(|m| m.method.clone())
            .collect()
    }
    fn call_fn_message(&self) -> CdpMessage {
        self.commands
            .lock()
            .iter()
            .find(|m| m.method == "Runtime.callFunctionOn")
            .cloned()
            .expect("a Runtime.callFunctionOn command was issued")
    }
}

/// TargetManager double: only `target_state` matters for inject; it returns a
/// state whose `audio_nonce` is configurable (Some by default, None to simulate
/// a non-`--audio` session).
struct FakeTargets {
    nonce: Option<String>,
}
impl FakeTargets {
    fn with_nonce() -> Self {
        Self {
            nonce: Some(NONCE.to_string()),
        }
    }
    fn without_nonce() -> Self {
        Self { nonce: None }
    }
}

#[async_trait]
impl TargetManager for FakeTargets {
    async fn create_new_target(
        &self,
        _session_id: SessionId,
        _profile: String,
        _seed: Seed,
        _epoch_ms: EpochMs,
        _determinism_enabled: bool,
        _audio_enabled: bool,
    ) -> Result<TargetId, TargetError> {
        Ok(TARGET)
    }
    fn target_for_session(&self, _session_id: SessionId) -> Option<TargetId> {
        Some(TARGET)
    }
    fn target_state(&self, target_id: TargetId) -> Option<TargetState> {
        // Only the REAL target (7) carries state; the `0` sentinel the host passes
        // does not — so inject must resolve via target_for_session or it fails.
        if target_id != TARGET {
            return None;
        }
        let mut st = TargetState::new(1, target_id, "default".to_string());
        st.audio_nonce = self.nonce.clone();
        Some(st)
    }
    fn close_target(&self, _target_id: TargetId) -> Result<(), TargetError> {
        Ok(())
    }
    fn invalidate_targets(&self) {}
    fn determinism_ready(&self, _target_id: TargetId) -> bool {
        true
    }
}

fn object_id_result(id: &str) -> CborValue {
    CborValue::Map(vec![(
        CborValue::Text("result".into()),
        CborValue::Map(vec![
            (
                CborValue::Text("type".into()),
                CborValue::Text("object".into()),
            ),
            (
                CborValue::Text("objectId".into()),
                CborValue::Text(id.into()),
            ),
        ]),
    )])
}

fn number_result(secs: f64) -> CborValue {
    CborValue::Map(vec![(
        CborValue::Text("result".into()),
        CborValue::Map(vec![
            (
                CborValue::Text("type".into()),
                CborValue::Text("number".into()),
            ),
            (CborValue::Text("value".into()), CborValue::Float(secs)),
        ]),
    )])
}

fn exception_result(message: &str) -> CborValue {
    CborValue::Map(vec![
        (
            CborValue::Text("result".into()),
            CborValue::Map(vec![(
                CborValue::Text("type".into()),
                CborValue::Text("object".into()),
            )]),
        ),
        (
            CborValue::Text("exceptionDetails".into()),
            CborValue::Map(vec![(
                CborValue::Text("exception".into()),
                CborValue::Map(vec![(
                    CborValue::Text("description".into()),
                    CborValue::Text(message.into()),
                )]),
            )]),
        ),
    ])
}

fn bridge(cdp: TestCdp, targets: FakeTargets) -> AudioBridge {
    AudioBridge::new(Arc::new(cdp), Arc::new(targets))
}

// ── task 05 capture helpers ──────────────────────────────────────────────────

/// Pull the `functionDeclaration` text from a `Runtime.callFunctionOn` params map.
fn func_declaration(params: &CborValue) -> Option<String> {
    if let CborValue::Map(entries) = params {
        for (k, v) in entries {
            if k == &CborValue::Text("functionDeclaration".into()) {
                if let CborValue::Text(s) = v {
                    return Some(s.clone());
                }
            }
        }
    }
    None
}

/// `{result:{value:{ok:true}}}` — the in-page start/stopCapture return.
fn ok_true_result() -> CborValue {
    CborValue::Map(vec![(
        CborValue::Text("result".into()),
        CborValue::Map(vec![(
            CborValue::Text("value".into()),
            CborValue::Map(vec![(CborValue::Text("ok".into()), CborValue::Bool(true))]),
        )]),
    )])
}

/// Build a scripted `callFunctionOn(drain)` response: `{result:{value:{…}}}`.
#[allow(clippy::too_many_arguments)]
fn drain_result(
    samples: &[f32],
    sample_rate: u32,
    dropped_frames: u64,
    tapped_tracks: u64,
    injected_leaked: bool,
    buffer_cap_hit: bool,
    more: bool,
) -> CborValue {
    let b64 = samples_to_b64(samples);
    CborValue::Map(vec![(
        CborValue::Text("result".into()),
        CborValue::Map(vec![(
            CborValue::Text("value".into()),
            CborValue::Map(vec![
                (CborValue::Text("samples_b64".into()), CborValue::Text(b64)),
                (
                    CborValue::Text("sample_rate".into()),
                    CborValue::Integer(i64::from(sample_rate).into()),
                ),
                (
                    CborValue::Text("dropped_frames".into()),
                    CborValue::Integer((dropped_frames as i64).into()),
                ),
                (
                    CborValue::Text("tapped_tracks".into()),
                    CborValue::Integer((tapped_tracks as i64).into()),
                ),
                (
                    CborValue::Text("injected_leaked".into()),
                    CborValue::Bool(injected_leaked),
                ),
                (
                    CborValue::Text("buffer_cap_hit".into()),
                    CborValue::Bool(buffer_cap_hit),
                ),
                (CborValue::Text("more".into()), CborValue::Bool(more)),
            ]),
        )]),
    )])
}

/// Encode mono f32 samples to little-endian bytes → base64 (the in-page wire form).
fn samples_to_b64(samples: &[f32]) -> String {
    let mut bytes = Vec::with_capacity(samples.len() * 4);
    for &s in samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    base64::engine::general_purpose::STANDARD.encode(&bytes)
}

/// A `freq` Hz sine, `n` samples at `rate` (unit amplitude).
fn sine(freq: f64, rate: u32, n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin() as f32)
        .collect()
}

/// Root-mean-square of an f32 slice.
fn rms(s: &[f32]) -> f64 {
    if s.is_empty() {
        return 0.0;
    }
    (s.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / s.len() as f64).sqrt()
}

/// Parse a returned WAV's `(sample_rate, channels, bits, sample_count, i16 body)`.
fn parse_wav(wav: &[u8]) -> (u32, u16, u16, usize, Vec<i16>) {
    assert_eq!(&wav[0..4], b"RIFF");
    assert_eq!(&wav[8..12], b"WAVE");
    let channels = u16::from_le_bytes([wav[22], wav[23]]);
    let rate = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
    let bits = u16::from_le_bytes([wav[34], wav[35]]);
    let data_len = u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]) as usize;
    let mut body = Vec::with_capacity(data_len / 2);
    for c in wav[44..44 + data_len].as_chunks::<2>().0 {
        body.push(i16::from_le_bytes(*c));
    }
    (rate, channels, bits, data_len / 2, body)
}

/// A clock that replays a scripted sequence of `elapsed_ms` values (saturating on
/// the last), so the drain-loop deadline can be driven deterministically.
struct FakeClock {
    values: Vec<u64>,
    idx: AtomicUsize,
}
impl FakeClock {
    fn new(values: Vec<u64>) -> Self {
        Self {
            values,
            idx: AtomicUsize::new(0),
        }
    }
}
impl AudioClock for FakeClock {
    fn elapsed_ms(&self) -> u64 {
        let i = self.idx.fetch_add(1, Ordering::Relaxed);
        *self
            .values
            .get(i)
            .or_else(|| self.values.last())
            .unwrap_or(&0)
    }
}

fn capture_bridge(cdp: TestCdp) -> AudioBridge {
    AudioBridge::new(Arc::new(cdp), Arc::new(FakeTargets::with_nonce()))
}

#[tokio::test]
async fn capture_tone_muxes_16k_mono_wav_preserving_rms() {
    // AC3 (shim boundary): a 440 Hz sine @ 48 kHz drained in one chunk →
    // stop_capture resamples to 16 kHz mono i16 and WAV-muxes. Parse the RIFF
    // header and assert rate/channels/bits + that RMS survives the pipeline. The
    // real tone-fidelity round-trip through Chromium is task 07.
    let cdp = TestCdp::default();
    let src = sine(440.0, 48_000, 4_800); // 0.1 s
    cdp.drain_queue
        .lock()
        .push_back(drain_result(&src, 48_000, 0, 1, false, false, false));
    let br = capture_bridge(cdp);

    br.start_capture(SESSION, TARGET_SENTINEL, Caps::default())
        .await
        .expect("start ok");
    let out = br.stop_capture(SESSION, TARGET_SENTINEL).await;

    assert_eq!(out.stop_reason, "explicit");
    assert_eq!(out.source_sample_rate, 48_000);
    assert_eq!(out.dropped_frames, 0);
    assert!(out.error.is_none());
    let (rate, channels, bits, count, body) = parse_wav(&out.wav_bytes);
    assert_eq!(rate, 16_000);
    assert_eq!(channels, 1);
    assert_eq!(bits, 16);
    assert_eq!(count as u64, out.sample_count);
    // 4800 @ 48 kHz → ~1600 @ 16 kHz.
    assert!((1550..=1650).contains(&count), "sample_count={count}");
    assert_eq!(out.duration_ms, out.sample_count * 1000 / 16_000);
    // RMS of the decoded i16 (back to f32) tracks the source sine RMS (~0.707).
    let decoded: Vec<f32> = body.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
    assert!((rms(&decoded) - rms(&src)).abs() < 1e-2, "RMS drifted");
    // The retired code path must never resurface.
    assert_ne!(out.stop_reason, "encoder_unavailable");
}

#[tokio::test]
async fn capture_drains_multiple_chunks_until_more_false() {
    let cdp = TestCdp::default();
    let a = sine(440.0, 16_000, 800);
    let b = sine(440.0, 16_000, 800);
    cdp.drain_queue
        .lock()
        .push_back(drain_result(&a, 16_000, 0, 1, false, false, true));
    cdp.drain_queue
        .lock()
        .push_back(drain_result(&b, 16_000, 0, 1, false, false, false));
    let br = capture_bridge(cdp.clone());
    br.start_capture(SESSION, TARGET_SENTINEL, Caps::default())
        .await
        .unwrap();
    let out = br.stop_capture(SESSION, TARGET_SENTINEL).await;
    // 16 kHz source is passthrough → 1600 samples total across two drains.
    assert_eq!(out.sample_count, 1600);
    assert_eq!(out.stop_reason, "explicit");
    // Two drain calls were issued (more:true then more:false).
    let drains = cdp
        .commands
        .lock()
        .iter()
        .filter(|m| {
            m.method == "Runtime.callFunctionOn"
                && func_declaration(&m.params)
                    .map(|d| d.contains("drain"))
                    .unwrap_or(false)
        })
        .count();
    assert_eq!(drains, 2);
}

#[tokio::test]
async fn byte_cap_truncates_with_stop_reason() {
    // AC9: caps truncate (never error). max_bytes = 1000 → 500 i16 samples.
    let cdp = TestCdp::default();
    let src = sine(440.0, 48_000, 4_800); // → ~1600 @ 16 kHz, over the 500 cap
    cdp.drain_queue
        .lock()
        .push_back(drain_result(&src, 48_000, 0, 1, false, false, false));
    let br = capture_bridge(cdp);
    br.start_capture(SESSION, TARGET_SENTINEL, Caps::sanitized(0, 1_000))
        .await
        .unwrap();
    let out = br.stop_capture(SESSION, TARGET_SENTINEL).await;
    assert_eq!(out.stop_reason, "byte_cap");
    // I2: the RETURNED WAV (header + body) must be bounded by max_bytes, so the
    // header is reserved: (1000 - 44) / 2 = 478 samples → 44 + 956 = 1000 bytes.
    assert_eq!(out.sample_count, 478);
    assert!(
        out.wav_bytes.len() as u64 <= 1_000,
        "wav must be ≤ max_bytes"
    );
    assert_eq!(out.wav_bytes.len(), 44 + 478 * 2);
    assert_ne!(out.stop_reason, "encoder_unavailable");
}

#[tokio::test]
async fn tighter_duration_cap_wins_when_both_apply() {
    // I1: both caps must combine — the tighter (duration) one wins. max_bytes = 4000
    // → (4000-44)/2 = 1978 sample limit; max_duration_ms = 25 → 400 sample limit.
    // Feed 6000 @ 16 kHz so BOTH caps are exceeded; duration is tighter → 400.
    let cdp = TestCdp::default();
    let src = sine(440.0, 16_000, 6_000);
    cdp.drain_queue
        .lock()
        .push_back(drain_result(&src, 16_000, 0, 1, false, false, false));
    let br = capture_bridge(cdp);
    br.start_capture(SESSION, TARGET_SENTINEL, Caps::sanitized(25, 4_000))
        .await
        .unwrap();
    let out = br.stop_capture(SESSION, TARGET_SENTINEL).await;
    assert_eq!(out.stop_reason, "duration_cap");
    assert_eq!(out.sample_count, 400); // 25 ms @ 16 kHz, NOT the 1978 byte limit
    assert_eq!(out.duration_ms, 25);
}

#[tokio::test]
async fn dropped_frames_carried_across_multiple_drains() {
    // M2: the page reports dropped_frames cumulatively; the final (more:false) drain
    // carries the running total. The shim must surface the max, never re-zero to the
    // last chunk's value.
    let cdp = TestCdp::default();
    let a = sine(440.0, 16_000, 400);
    let b = sine(440.0, 16_000, 400);
    cdp.drain_queue
        .lock()
        .push_back(drain_result(&a, 16_000, 2, 1, false, false, true));
    cdp.drain_queue
        .lock()
        .push_back(drain_result(&b, 16_000, 5, 1, false, false, false));
    let br = capture_bridge(cdp);
    br.start_capture(SESSION, TARGET_SENTINEL, Caps::default())
        .await
        .unwrap();
    let out = br.stop_capture(SESSION, TARGET_SENTINEL).await;
    assert_eq!(out.dropped_frames, 5);
    assert_eq!(out.sample_count, 800);
    assert_eq!(out.stop_reason, "explicit");
}

#[tokio::test]
async fn duration_cap_truncates_with_stop_reason() {
    // AC9: max_duration_ms = 50 → 800 samples @ 16 kHz. Feed ~100 ms.
    let cdp = TestCdp::default();
    let src = sine(440.0, 16_000, 1_600); // 100 ms @ 16 kHz passthrough
    cdp.drain_queue
        .lock()
        .push_back(drain_result(&src, 16_000, 0, 1, false, false, false));
    let br = capture_bridge(cdp);
    br.start_capture(SESSION, TARGET_SENTINEL, Caps::sanitized(50, 0))
        .await
        .unwrap();
    let out = br.stop_capture(SESSION, TARGET_SENTINEL).await;
    assert_eq!(out.stop_reason, "duration_cap");
    assert_eq!(out.sample_count, 800);
    assert_eq!(out.duration_ms, 50);
}

#[tokio::test]
async fn in_page_buffer_cap_hit_maps_to_byte_cap() {
    // C2: the in-page ring hitting its hard ceiling is a truncation the caller
    // must be told about, not a silent "explicit".
    let cdp = TestCdp::default();
    let src = sine(440.0, 16_000, 800);
    cdp.drain_queue
        .lock()
        .push_back(drain_result(&src, 16_000, 0, 1, false, true, false));
    let br = capture_bridge(cdp);
    br.start_capture(SESSION, TARGET_SENTINEL, Caps::default())
        .await
        .unwrap();
    let out = br.stop_capture(SESSION, TARGET_SENTINEL).await;
    assert_eq!(out.stop_reason, "byte_cap");
}

#[tokio::test]
async fn injected_leak_tripwire_maps_to_error() {
    // AC9 / A2 (shim-side cross-check): if the page ever reports our own outbound
    // track reaching the tap, the capture is a typed error, not returned audio.
    let cdp = TestCdp::default();
    let src = sine(440.0, 16_000, 800);
    cdp.drain_queue
        .lock()
        .push_back(drain_result(&src, 16_000, 0, 1, true, false, false));
    let br = capture_bridge(cdp);
    br.start_capture(SESSION, TARGET_SENTINEL, Caps::default())
        .await
        .unwrap();
    let out = br.stop_capture(SESSION, TARGET_SENTINEL).await;
    assert_eq!(out.stop_reason, "error");
    assert_eq!(out.error.as_deref(), Some("injected_track_leaked"));
    assert!(out.wav_bytes.is_empty());
}

#[tokio::test]
async fn no_inbound_track_vs_no_samples_are_distinct() {
    // I1: zero samples with tapped_tracks == 0 → no_inbound_track (WebRTC never
    // connected); with a track seen but silent → no_samples.
    let cdp = TestCdp::default();
    cdp.drain_queue
        .lock()
        .push_back(drain_result(&[], 0, 0, 0, false, false, false));
    let br = capture_bridge(cdp);
    br.start_capture(SESSION, TARGET_SENTINEL, Caps::default())
        .await
        .unwrap();
    let out = br.stop_capture(SESSION, TARGET_SENTINEL).await;
    assert_eq!(out.stop_reason, "no_inbound_track");

    let cdp2 = TestCdp::default();
    cdp2.drain_queue
        .lock()
        .push_back(drain_result(&[], 16_000, 0, 1, false, false, false));
    let br2 = capture_bridge(cdp2);
    br2.start_capture(SESSION, TARGET_SENTINEL, Caps::default())
        .await
        .unwrap();
    let out2 = br2.stop_capture(SESSION, TARGET_SENTINEL).await;
    assert_eq!(out2.stop_reason, "no_samples");
}

#[tokio::test]
async fn stop_before_start_is_distinct_error() {
    // I2: stop with no active capture is its own typed error (mirrors screencast),
    // NOT a no_samples outcome.
    let cdp = TestCdp::default();
    let br = capture_bridge(cdp);
    let out = br.stop_capture(SESSION, TARGET_SENTINEL).await;
    assert_eq!(out.stop_reason, "error");
    assert!(out
        .error
        .as_deref()
        .unwrap_or("")
        .contains("no active capture"));
}

#[tokio::test]
async fn double_start_is_capture_already_active() {
    let cdp = TestCdp::default();
    let br = capture_bridge(cdp);
    br.start_capture(SESSION, TARGET_SENTINEL, Caps::default())
        .await
        .expect("first start ok");
    let err = br
        .start_capture(SESSION, TARGET_SENTINEL, Caps::default())
        .await
        .expect_err("second start must fail");
    assert!(err.starts_with("capture_already_active"), "got {err}");
    assert!(br.is_capturing(SESSION, TARGET_SENTINEL));
}

#[tokio::test]
async fn is_capturing_tracks_lifecycle() {
    let cdp = TestCdp::default();
    cdp.drain_queue.lock().push_back(drain_result(
        &sine(440.0, 16_000, 800),
        16_000,
        0,
        1,
        false,
        false,
        false,
    ));
    let br = capture_bridge(cdp);
    assert!(!br.is_capturing(SESSION, TARGET_SENTINEL));
    br.start_capture(SESSION, TARGET_SENTINEL, Caps::default())
        .await
        .unwrap();
    assert!(br.is_capturing(SESSION, TARGET_SENTINEL));
    let _ = br.stop_capture(SESSION, TARGET_SENTINEL).await;
    assert!(!br.is_capturing(SESSION, TARGET_SENTINEL));
}

#[tokio::test]
async fn start_capture_without_audio_is_audio_not_enabled() {
    let cdp = TestCdp::default();
    let br = AudioBridge::new(
        Arc::new(cdp.clone()),
        Arc::new(FakeTargets::without_nonce()),
    );
    let err = br
        .start_capture(SESSION, TARGET_SENTINEL, Caps::default())
        .await
        .unwrap_err();
    assert!(err.starts_with("audio_not_enabled"), "got {err}");
    assert!(cdp.methods().is_empty(), "no CDP issued without --audio");
}

#[tokio::test]
async fn drain_loop_honors_deadline_and_reports_timeout() {
    // The drain deadline bounds a page that keeps returning `more:true`. The fake
    // clock reads 0 (start), 0 (first check → one drain), then jumps past the 10 s
    // deadline so the loop exits with capture_drain_timeout.
    let cdp = TestCdp::default();
    for _ in 0..4 {
        cdp.drain_queue.lock().push_back(drain_result(
            &sine(440.0, 16_000, 100),
            16_000,
            0,
            1,
            false,
            false,
            true, // never done
        ));
    }
    let clock = Arc::new(FakeClock::new(vec![0, 0, 20_000]));
    let br = AudioBridge::with_clock(Arc::new(cdp), Arc::new(FakeTargets::with_nonce()), clock);
    br.start_capture(SESSION, TARGET_SENTINEL, Caps::default())
        .await
        .unwrap();
    let out = br.stop_capture(SESSION, TARGET_SENTINEL).await;
    assert_eq!(out.stop_reason, "error");
    assert_eq!(out.error.as_deref(), Some("capture_drain_timeout"));
}

#[tokio::test]
async fn inject_resolves_object_then_calls_enqueue_with_bytes_as_argument() {
    let cdp = TestCdp::default();
    let br = bridge(cdp.clone(), FakeTargets::with_nonce());

    let audio = b"RIFF....WAVEfake-pcm-bytes";
    let outcome = br
        .inject(SESSION, TARGET_SENTINEL, audio, false)
        .await
        .expect("inject ok");
    assert_eq!(outcome.duration_ms, 800);
    assert!(!outcome.awaited_playout);

    // Ordered: evaluate (resolve objectId) → callFunctionOn → releaseObject
    // (the handle is freed so repeated injects don't leak renderer objects).
    assert_eq!(
        cdp.methods(),
        vec![
            "Runtime.evaluate",
            "Runtime.callFunctionOn",
            "Runtime.releaseObject"
        ]
    );

    // A3: the audio bytes must appear as a CallArgument, NEVER inside the
    // functionDeclaration source.
    let call = cdp.call_fn_message();
    let CborValue::Map(params) = &call.params else {
        panic!("callFunctionOn params must be a map");
    };
    let get = |k: &str| {
        params
            .iter()
            .find(|(pk, _)| pk == &CborValue::Text(k.into()))
            .map(|(_, v)| v)
    };

    let decl = match get("functionDeclaration") {
        Some(CborValue::Text(s)) => s.clone(),
        other => panic!("functionDeclaration must be text, got {other:?}"),
    };
    let expected_b64 = base64::engine::general_purpose::STANDARD.encode(audio);
    assert!(
        !decl.contains(&expected_b64),
        "audio bytes must NOT be interpolated into the JS declaration (A3)"
    );

    let args = match get("arguments") {
        Some(CborValue::Array(a)) => a.clone(),
        other => panic!("arguments must be an array, got {other:?}"),
    };
    assert_eq!(args.len(), 2, "enqueue(b64, awaitPlayout)");
    // arg0.value == the base64 string of the exact bytes.
    let arg0_val = match &args[0] {
        CborValue::Map(m) => m
            .iter()
            .find(|(k, _)| k == &CborValue::Text("value".into()))
            .map(|(_, v)| v.clone()),
        _ => None,
    };
    assert_eq!(
        arg0_val,
        Some(CborValue::Text(expected_b64)),
        "arg0 carries the exact b64 bytes"
    );
    // arg1.value == await_playout (false here).
    let arg1_val = match &args[1] {
        CborValue::Map(m) => m
            .iter()
            .find(|(k, _)| k == &CborValue::Text("value".into()))
            .map(|(_, v)| v.clone()),
        _ => None,
    };
    assert_eq!(
        arg1_val,
        Some(CborValue::Bool(false)),
        "arg1 carries await_playout"
    );

    // awaitPromise must be true so enqueue rejections surface + await_playout works.
    assert_eq!(get("awaitPromise"), Some(&CborValue::Bool(true)));
}

#[tokio::test]
async fn await_playout_flag_is_threaded_into_the_call() {
    let cdp = TestCdp::default();
    let br = bridge(cdp.clone(), FakeTargets::with_nonce());
    let outcome = br
        .inject(SESSION, TARGET_SENTINEL, b"clip", true)
        .await
        .expect("inject ok");
    assert!(outcome.awaited_playout);

    let call = cdp.call_fn_message();
    let CborValue::Map(params) = &call.params else {
        panic!("map")
    };
    let args = params
        .iter()
        .find(|(k, _)| k == &CborValue::Text("arguments".into()))
        .and_then(|(_, v)| match v {
            CborValue::Array(a) => Some(a.clone()),
            _ => None,
        })
        .expect("arguments array");
    let arg1_val = match &args[1] {
        CborValue::Map(m) => m
            .iter()
            .find(|(k, _)| k == &CborValue::Text("value".into()))
            .map(|(_, v)| v.clone()),
        _ => None,
    };
    assert_eq!(
        arg1_val,
        Some(CborValue::Bool(true)),
        "await_playout=true threaded"
    );
}

#[tokio::test]
async fn five_successive_injects_all_succeed_no_state_carried() {
    // AC2: repeatability — the bridge is stateless per-call, so ≥5 injections in
    // one session all succeed without any relaunch/reset. Each issues exactly the
    // evaluate+callFunctionOn pair.
    let cdp = TestCdp::default();
    let br = bridge(cdp.clone(), FakeTargets::with_nonce());
    for i in 0..5 {
        br.inject(
            SESSION,
            TARGET_SENTINEL,
            format!("utterance-{i}").as_bytes(),
            false,
        )
        .await
        .unwrap_or_else(|e| panic!("inject {i} failed: {e}"));
    }
    assert_eq!(
        cdp.methods().len(),
        15,
        "5 injects × (evaluate + callFunctionOn + releaseObject)"
    );
}

#[tokio::test]
async fn missing_nonce_is_audio_not_enabled_and_issues_no_cdp() {
    let cdp = TestCdp::default();
    let br = bridge(cdp.clone(), FakeTargets::without_nonce());
    let err = br
        .inject(SESSION, TARGET_SENTINEL, b"clip", false)
        .await
        .unwrap_err();
    assert!(err.starts_with("audio_not_enabled"), "got {err}");
    assert!(
        cdp.methods().is_empty(),
        "no CDP issued when audio isn't enabled"
    );
}

#[tokio::test]
async fn enqueue_rejection_maps_no_microphone_request() {
    let cdp = TestCdp::default();
    *cdp.callfn_response.lock() = exception_result("Error: no_microphone_request");
    let br = bridge(cdp.clone(), FakeTargets::with_nonce());
    let err = br
        .inject(SESSION, TARGET_SENTINEL, b"clip", false)
        .await
        .unwrap_err();
    assert_eq!(err, "no_microphone_request");
}

#[tokio::test]
async fn enqueue_decode_rejection_maps_audio_decode_failed() {
    let cdp = TestCdp::default();
    *cdp.callfn_response.lock() = exception_result("EncodingError: Unable to decode audio data");
    let br = bridge(cdp.clone(), FakeTargets::with_nonce());
    let err = br
        .inject(SESSION, TARGET_SENTINEL, b"clip", false)
        .await
        .unwrap_err();
    assert_eq!(err, "audio_decode_failed");
}

#[tokio::test]
async fn absent_api_global_is_audio_bridge_unavailable() {
    let cdp = TestCdp::default();
    *cdp.evaluate_missing.lock() = true;
    let br = bridge(cdp.clone(), FakeTargets::with_nonce());
    let err = br
        .inject(SESSION, TARGET_SENTINEL, b"clip", false)
        .await
        .unwrap_err();
    assert!(err.starts_with("audio_bridge_unavailable"), "got {err}");
}

#[tokio::test]
async fn cdp_timeout_maps_inject_timeout() {
    let cdp = TestCdp::default();
    *cdp.force_error.lock() = Some(CdpError::Timeout { ms: 5000 });
    let br = bridge(cdp.clone(), FakeTargets::with_nonce());
    let err = br
        .inject(SESSION, TARGET_SENTINEL, b"clip", false)
        .await
        .unwrap_err();
    assert!(err.starts_with("inject_timeout"), "got {err}");
}
