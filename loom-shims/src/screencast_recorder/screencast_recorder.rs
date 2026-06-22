// ScreencastRecorder — per-target video recording over CDP Page.startScreencast.
//
// Lifecycle (one active recording per target):
//   start(target, caps): Page.startScreencast(jpeg) → register a
//     `Page.screencastFrame` handler that base64-decodes + buffers each frame
//     (under the byte cap) and queues its sessionId for ack → spawn an async
//     task that acks each frame (keeping the stream flowing) and enforces the
//     duration cap (auto-stop).
//   stop(target): Page.stopScreencast → join the ack task → encode the buffered
//     JPEG frames to a .webm via the injected encoder → ScreencastOutcome.
//
// The encoder is a trait so tests inject a deterministic fake and never need a
// real ffmpeg. The default `FfmpegSidecarEncoder` lazily auto-downloads a
// per-platform ffmpeg (runtime, ~100MB, cached) — so the loom build stays free
// of any C/ffmpeg build dependency. Recording is best-effort: any encode/encoder
// failure surfaces as `ScreencastOutcome.error` (the caller emits an error
// receipt) and NEVER aborts the session.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use ciborium::value::Value as CborValue;
use dashmap::DashMap;
use parking_lot::Mutex;
use tokio::task::JoinHandle;

use crate::cdp_connection::{CdpConnection, EventFilter, EventHandler, EventRegistration};
use loom_shared::navigate_outcome::ScreencastOutcome;
use loom_shared::shim_protocol::{CdpMessage, TargetId};

/// Env kill-switch: `LOOM_DISABLE_RECORDING=1` makes `start` refuse without a
/// recompile (runtime rollback per plan R3/D4). Checked per-call so an operator
/// can flip it on a running daemon.
fn recording_disabled() -> bool {
    matches!(std::env::var("LOOM_DISABLE_RECORDING"), Ok(v) if v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Resource caps for one recording. `0` disables a cap.
#[derive(Debug, Clone, Copy)]
pub struct Caps {
    pub max_duration_ms: u64,
    pub max_bytes: u64,
    pub frame_rate: u32,
}

impl Default for Caps {
    fn default() -> Self {
        // Safe defaults (plan D5). Overridable per `web.start_recording`.
        Caps {
            max_duration_ms: 300_000, // 5 min
            max_bytes: 268_435_456,   // 256 MiB
            frame_rate: 10,
        }
    }
}

impl Caps {
    /// Build from optional wire overrides, falling back to safe defaults.
    ///
    /// A `Some(0)` (or `None`) for the duration/bytes caps maps to the safe
    /// DEFAULT, not "unlimited" — the public API (`web.start_recording`) must not
    /// be able to disable the safety caps by passing 0 (ship-council CRIT). The
    /// internal [`Caps`] literal can still set 0 to intentionally disable a cap
    /// (used only by tests).
    pub fn from_overrides(
        max_duration_ms: Option<u64>,
        max_bytes: Option<u64>,
        frame_rate: Option<u64>,
    ) -> Self {
        Self::sanitized(
            max_duration_ms.unwrap_or(0),
            max_bytes.unwrap_or(0),
            frame_rate.unwrap_or(0),
        )
    }

    /// Build from concrete wire values, clamping out-of-range/disabling inputs to
    /// safe defaults. `0` (or absurd) duration/bytes → default; frame_rate is
    /// clamped to `1..=60`. This is the ONLY constructor the daemon/dispatcher
    /// use, so a caller can never disable the caps through the public API.
    pub fn sanitized(max_duration_ms: u64, max_bytes: u64, frame_rate: u64) -> Self {
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
            frame_rate: if frame_rate == 0 {
                d.frame_rate
            } else {
                frame_rate.clamp(1, 60) as u32
            },
        }
    }
}

/// Encodes buffered JPEG frames into a `.webm`. Injected so tests don't need
/// ffmpeg; the production impl is [`FfmpegSidecarEncoder`].
pub trait ScreencastEncoder: Send + Sync {
    /// `frames` are JPEG byte blobs in capture order. Returns the `.webm` bytes
    /// or a human-readable error (best-effort — never panics).
    fn encode(&self, frames: &[Vec<u8>], frame_rate: u32) -> Result<Vec<u8>, String>;
}

/// Per-target recording state. Dropping it deregisters the frame handler (via
/// `_reg`) — so a recording cannot outlive its entry in the active map.
struct RecordingState {
    frames: Arc<Mutex<Vec<Vec<u8>>>>,
    frame_rate: u32,
    started: Instant,
    stop_flag: Arc<AtomicBool>,
    stop_reason: Arc<Mutex<String>>,
    ack_task: JoinHandle<()>,
    _reg: EventRegistration,
}

/// Host/shim-side recorder. One per shim process; keyed by target.
pub struct ScreencastRecorder {
    cdp: Arc<dyn CdpConnection>,
    encoder: Arc<dyn ScreencastEncoder>,
    active: DashMap<TargetId, RecordingState>,
}

impl ScreencastRecorder {
    pub fn new(cdp: Arc<dyn CdpConnection>, encoder: Arc<dyn ScreencastEncoder>) -> Self {
        Self {
            cdp,
            encoder,
            active: DashMap::new(),
        }
    }

    /// True if a recording is active for `target_id`.
    pub fn is_recording(&self, target_id: TargetId) -> bool {
        self.active.contains_key(&target_id)
    }

    /// Start a recording. Errors (no session abort) on: kill-switch set, a
    /// recording already active for the target, or the `Page.startScreencast`
    /// command failing.
    pub async fn start(&self, target_id: TargetId, caps: Caps) -> Result<(), String> {
        if recording_disabled() {
            return Err("recording disabled via LOOM_DISABLE_RECORDING".to_string());
        }
        if self.active.contains_key(&target_id) {
            return Err("a recording is already active for this target".to_string());
        }

        let frames: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
        let total_bytes = Arc::new(AtomicU64::new(0));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_reason = Arc::new(Mutex::new(String::new()));
        let (ack_tx, mut ack_rx) = tokio::sync::mpsc::unbounded_channel::<i64>();

        // Frame handler (sync): decode + buffer under the byte cap, then queue
        // the sessionId for the ack task. We always queue an ack — even for a
        // dropped (over-cap) frame — so Chrome keeps draining rather than
        // stalling the stream mid-recording.
        let max_bytes = caps.max_bytes;
        let h_frames = frames.clone();
        let h_bytes = total_bytes.clone();
        let h_reason = stop_reason.clone();
        let handler: EventHandler = Arc::new(move |_t: TargetId, msg: CdpMessage| {
            if msg.method != "Page.screencastFrame" {
                return;
            }
            if let Some((data_b64, session_id)) = extract_screencast_frame(&msg.params) {
                if let Ok(jpeg) =
                    base64::engine::general_purpose::STANDARD.decode(data_b64.as_bytes())
                {
                    let cur = h_bytes.load(Ordering::Relaxed);
                    if max_bytes == 0 || cur + jpeg.len() as u64 <= max_bytes {
                        h_bytes.fetch_add(jpeg.len() as u64, Ordering::Relaxed);
                        h_frames.lock().push(jpeg);
                    } else {
                        let mut r = h_reason.lock();
                        if r.is_empty() {
                            *r = "byte_cap".to_string();
                        }
                    }
                }
                let _ = ack_tx.send(session_id);
            }
        });
        let reg = self
            .cdp
            .register_event_handler(EventFilter::new("Page.screencastFrame"), handler);

        // Ack task: drains queued sessionIds and acks each (CDP backpressure),
        // and enforces the duration cap by auto-stopping the screencast.
        let cdp = self.cdp.clone();
        let t_stop = stop_flag.clone();
        let t_reason = stop_reason.clone();
        let max_duration_ms = caps.max_duration_ms;
        let started = Instant::now();
        let ack_task = tokio::spawn(async move {
            loop {
                if t_stop.load(Ordering::Relaxed) {
                    break;
                }
                match tokio::time::timeout(Duration::from_millis(20), ack_rx.recv()).await {
                    Ok(Some(session_id)) => {
                        let ack = CdpMessage {
                            method: "Page.screencastFrameAck".to_string(),
                            params: CborValue::Map(vec![(
                                CborValue::Text("sessionId".to_string()),
                                CborValue::Integer(session_id.into()),
                            )]),
                        };
                        let _ = cdp
                            .command(target_id, ack, Some(Duration::from_secs(2)))
                            .await;
                    }
                    Ok(None) => break, // sender dropped
                    Err(_) => {}       // recv timeout — fall through to cap check
                }
                if max_duration_ms > 0 && started.elapsed().as_millis() as u64 >= max_duration_ms {
                    {
                        let mut r = t_reason.lock();
                        if r.is_empty() {
                            *r = "duration_cap".to_string();
                        }
                    }
                    let stop = CdpMessage {
                        method: "Page.stopScreencast".to_string(),
                        params: CborValue::Map(vec![]),
                    };
                    let _ = cdp
                        .command(target_id, stop, Some(Duration::from_secs(2)))
                        .await;
                    break;
                }
            }
        });

        // Begin the screencast. Dimensions/quality are fixed safe defaults
        // (plan D5/R12); only duration/bytes/fps are caller-tunable.
        let start_msg = CdpMessage {
            method: "Page.startScreencast".to_string(),
            params: CborValue::Map(vec![
                (
                    CborValue::Text("format".to_string()),
                    CborValue::Text("jpeg".to_string()),
                ),
                (
                    CborValue::Text("quality".to_string()),
                    CborValue::Integer(70.into()),
                ),
                (
                    CborValue::Text("maxWidth".to_string()),
                    CborValue::Integer(1280.into()),
                ),
                (
                    CborValue::Text("maxHeight".to_string()),
                    CborValue::Integer(1280.into()),
                ),
                (
                    CborValue::Text("everyNthFrame".to_string()),
                    CborValue::Integer(1.into()),
                ),
            ]),
        };
        // Reserve the target slot ATOMICALLY before the await — two concurrent
        // start()s for the same target cannot both pass (ship-council: the
        // contains_key→insert TOCTOU). The DashMap entry lock is held only across
        // the synchronous insert, never across an await.
        match self.active.entry(target_id) {
            dashmap::mapref::entry::Entry::Occupied(_) => {
                stop_flag.store(true, Ordering::Relaxed);
                ack_task.abort();
                // `reg` drops at return → handler deregistered.
                return Err("a recording is already active for this target".to_string());
            }
            dashmap::mapref::entry::Entry::Vacant(slot) => {
                slot.insert(RecordingState {
                    frames,
                    frame_rate: caps.frame_rate,
                    started,
                    stop_flag,
                    stop_reason,
                    ack_task,
                    _reg: reg,
                });
            }
        }

        // Begin the screencast now that the slot is reserved. On failure, undo
        // the reservation + tear down the ack task / handler.
        if let Err(e) = self
            .cdp
            .command(target_id, start_msg, Some(Duration::from_secs(5)))
            .await
        {
            if let Some((_, st)) = self.active.remove(&target_id) {
                st.stop_flag.store(true, Ordering::Relaxed);
                st.ack_task.abort();
            }
            return Err(format!("Page.startScreencast failed: {e}"));
        }
        Ok(())
    }

    /// Stop the active recording and encode it. Always returns a
    /// `ScreencastOutcome` (never errors at the call boundary): a missing
    /// recording, zero frames, or an encoder failure are reported via
    /// `stop_reason` + `error` so the daemon can emit an error receipt without
    /// aborting the session.
    pub async fn stop(&self, target_id: TargetId) -> ScreencastOutcome {
        let Some((_, state)) = self.active.remove(&target_id) else {
            return ScreencastOutcome {
                stop_reason: "error".to_string(),
                error: Some("no active recording for this target".to_string()),
                ..Default::default()
            };
        };
        let RecordingState {
            frames,
            frame_rate,
            started,
            stop_flag,
            stop_reason,
            ack_task,
            _reg,
        } = state;

        // Stop the stream (idempotent if the duration cap already auto-stopped).
        let stop_msg = CdpMessage {
            method: "Page.stopScreencast".to_string(),
            params: CborValue::Map(vec![]),
        };
        let _ = self
            .cdp
            .command(target_id, stop_msg, Some(Duration::from_secs(2)))
            .await;
        stop_flag.store(true, Ordering::Relaxed);
        let _ = ack_task.await;

        let collected = std::mem::take(&mut *frames.lock());
        let duration_ms = started.elapsed().as_millis() as u64;
        let frame_count = collected.len() as u64;
        let mut reason = stop_reason.lock().clone();
        if reason.is_empty() {
            reason = "explicit".to_string();
        }

        if collected.is_empty() {
            return ScreencastOutcome {
                webm_bytes: Vec::new(),
                frame_count: 0,
                duration_ms,
                stop_reason: "no_frames".to_string(),
                error: Some("recording captured zero frames".to_string()),
            };
        }

        match self.encoder.encode(&collected, frame_rate) {
            Ok(webm) => ScreencastOutcome {
                webm_bytes: webm,
                frame_count,
                duration_ms,
                stop_reason: reason,
                error: None,
            },
            Err(e) => {
                // A best-effort encode failure (e.g. flaky ffmpeg download) must NOT
                // overwrite the true stop reason — `stop_reason` answers "why did
                // recording stop" (cap/duration/explicit), independent of whether the
                // encode later succeeded. The failure is surfaced via `error` + empty
                // `webm_bytes` (the daemon emits an error receipt from those).
                tracing::warn!(
                    target_id,
                    error = %e,
                    stop_reason = %reason,
                    "screencast encode failed; preserving true stop_reason, surfacing failure in `error`"
                );
                ScreencastOutcome {
                    webm_bytes: Vec::new(),
                    frame_count,
                    duration_ms,
                    stop_reason: reason,
                    error: Some(e),
                }
            }
        }
    }
}

/// Extract `(data_base64, session_id)` from a `Page.screencastFrame` event's
/// CBOR params (`{data: <base64>, sessionId: <int>, metadata: {...}}`).
fn extract_screencast_frame(params: &CborValue) -> Option<(String, i64)> {
    let CborValue::Map(entries) = params else {
        return None;
    };
    let mut data: Option<String> = None;
    let mut session_id: Option<i64> = None;
    for (k, v) in entries {
        if let CborValue::Text(key) = k {
            match key.as_str() {
                "data" => {
                    if let CborValue::Text(s) = v {
                        data = Some(s.clone());
                    }
                }
                "sessionId" => {
                    if let CborValue::Integer(i) = v {
                        session_id = i64::try_from(*i).ok();
                    }
                }
                _ => {}
            }
        }
    }
    Some((data?, session_id?))
}

/// Production encoder: JPEG frames → `.webm` (VP9). Writes the frames to a temp
/// dir and runs ffmpeg with a file output, so the pipeline is bounded by the
/// recording's byte cap. Prefers a **system ffmpeg** on `PATH` (fast, offline,
/// common on dev/CI); only if none is found does it fall back to
/// `ffmpeg-sidecar`'s runtime auto-download (cached, ~100MB) — so the loom build
/// carries NO C/ffmpeg build dependency either way.
pub struct FfmpegSidecarEncoder;

/// Resolve an ffmpeg binary: a system `ffmpeg` if it answers `-version`, else a
/// sidecar-downloaded one. Returns the program to pass to `Command::new`.
fn resolve_ffmpeg() -> Result<std::ffi::OsString, String> {
    let system_ok = std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if system_ok {
        return Ok(std::ffi::OsString::from("ffmpeg"));
    }
    // No system ffmpeg — download a per-platform one (cached after first use).
    ffmpeg_sidecar::download::auto_download().map_err(|e| {
        format!(
            "ffmpeg unavailable (no system ffmpeg on PATH; auto-download failed): {e}. \
             Install ffmpeg or set LOOM_DISABLE_RECORDING=1"
        )
    })?;
    Ok(ffmpeg_sidecar::paths::ffmpeg_path().into_os_string())
}

impl ScreencastEncoder for FfmpegSidecarEncoder {
    fn encode(&self, frames: &[Vec<u8>], frame_rate: u32) -> Result<Vec<u8>, String> {
        use std::io::Write as _;

        let ffmpeg = resolve_ffmpeg()?;
        let dir = tempfile::tempdir().map_err(|e| format!("temp dir failed: {e}"))?;
        for (i, frame) in frames.iter().enumerate() {
            let path = dir.path().join(format!("frame_{:05}.jpg", i + 1));
            let mut f =
                std::fs::File::create(&path).map_err(|e| format!("write frame failed: {e}"))?;
            f.write_all(frame)
                .map_err(|e| format!("write frame failed: {e}"))?;
        }
        let pattern = dir.path().join("frame_%05d.jpg");
        let out = dir.path().join("out.webm");
        let fr = frame_rate.max(1).to_string();

        let mut child = std::process::Command::new(&ffmpeg)
            .args([
                std::ffi::OsStr::new("-y"),
                std::ffi::OsStr::new("-framerate"),
                std::ffi::OsStr::new(&fr),
                std::ffi::OsStr::new("-i"),
                pattern.as_os_str(),
                std::ffi::OsStr::new("-c:v"),
                std::ffi::OsStr::new("libvpx-vp9"),
                std::ffi::OsStr::new("-pix_fmt"),
                std::ffi::OsStr::new("yuv420p"),
                std::ffi::OsStr::new("-an"),
                out.as_os_str(),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("ffmpeg spawn failed: {e}"))?;

        // Bound the encode wall-clock — a hung ffmpeg must NOT block the shim
        // indefinitely (plan R2 / ship-council CRIT). Kept under the host's
        // stop_recording recv timeout (120s) so the encoder reports the timeout
        // itself rather than the host giving up first.
        let deadline = Instant::now() + Duration::from_secs(ENCODE_TIMEOUT_SECS);
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(format!(
                            "ffmpeg encode timed out after {ENCODE_TIMEOUT_SECS}s (killed)"
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(format!("ffmpeg wait failed: {e}")),
            }
        };
        if !status.success() {
            let mut stderr = String::new();
            if let Some(mut e) = child.stderr.take() {
                use std::io::Read as _;
                let _ = e.read_to_string(&mut stderr);
            }
            let tail: String = stderr.lines().rev().take(3).collect::<Vec<_>>().join(" | ");
            return Err(format!("ffmpeg exited with {status}: {tail}"));
        }
        std::fs::read(&out).map_err(|e| format!("read encoded webm failed: {e}"))
    }
}

/// Wall-clock cap on a single ffmpeg encode (seconds). Below the host's
/// `stop_recording` recv timeout so the encoder surfaces the timeout itself.
const ENCODE_TIMEOUT_SECS: u64 = 90;
