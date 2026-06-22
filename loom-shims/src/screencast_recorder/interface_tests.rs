//! Recorder integration tests — drive the full start → frames → ack → stop →
//! encode flow against a `TestCdp` double (no real Chromium / ffmpeg). Covers
//! the happy path, the per-frame ack (CDP backpressure), the caps (byte +
//! duration), and every best-effort error edge (no-start, double-start,
//! zero-frame, encoder failure, kill-switch). The real-browser + real-ffmpeg
//! path is exercised by the `#[ignore]`d e2e in `loom-host`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use ciborium::value::Value as CborValue;
use parking_lot::Mutex;

use super::screencast_recorder::{
    Caps, FfmpegSidecarEncoder, ScreencastEncoder, ScreencastRecorder,
};
use crate::cdp_connection::{
    CdpConnection, CdpError, EventFilter, EventHandler, EventRegistration,
};
use loom_shared::shim_protocol::{CdpMessage, TargetId};

/// CDP double: records issued command methods and the registered frame
/// handlers, and lets a test fire synthetic `Page.screencastFrame` events.
#[derive(Clone, Default)]
struct TestCdp {
    commands: Arc<Mutex<Vec<String>>>,
    handlers: Arc<Mutex<Vec<EventHandler>>>,
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
        self.commands.lock().push(msg.method.clone());
        Ok(CborValue::Null)
    }
    fn register_event_handler(
        &self,
        _filter: EventFilter,
        handler: EventHandler,
    ) -> EventRegistration {
        self.handlers.lock().push(handler);
        EventRegistration::detached(0)
    }
    fn invalidate_session(&self) {}
    fn is_connected(&self) -> bool {
        true
    }
}

impl TestCdp {
    fn fire_frame(&self, payload: &[u8], session_id: i64) {
        let data = base64::engine::general_purpose::STANDARD.encode(payload);
        let msg = CdpMessage {
            method: "Page.screencastFrame".to_string(),
            params: CborValue::Map(vec![
                (CborValue::Text("data".to_string()), CborValue::Text(data)),
                (
                    CborValue::Text("sessionId".to_string()),
                    CborValue::Integer(session_id.into()),
                ),
            ]),
        };
        for h in self.handlers.lock().iter() {
            h(7, msg.clone());
        }
    }
    fn command_count(&self, method: &str) -> usize {
        self.commands.lock().iter().filter(|m| *m == method).count()
    }
}

/// Encoder double: returns a fixed marker or a forced error.
struct FakeEncoder {
    result: Result<Vec<u8>, String>,
}
impl ScreencastEncoder for FakeEncoder {
    fn encode(&self, _frames: &[Vec<u8>], _frame_rate: u32) -> Result<Vec<u8>, String> {
        self.result.clone()
    }
}

fn recorder(cdp: TestCdp, enc: Result<Vec<u8>, String>) -> ScreencastRecorder {
    ScreencastRecorder::new(Arc::new(cdp), Arc::new(FakeEncoder { result: enc }))
}

/// Serializes tests that read or mutate the process-global `LOOM_DISABLE_RECORDING` env:
/// `kill_switch_blocks_start` SETS it for its window, while every recording-start test READS
/// it inside `start()`. Env is per-process, so under parallel execution the kill-switch test
/// would make a concurrent `start().expect("start ok")` see recording disabled and fail. Each
/// such test holds this lock (`ENV_LOCK.lock().await`) for its duration. A `tokio::sync::Mutex`
/// (not `std`) so the guard can be held across `.await` without tripping `await_holding_lock`.
/// nextest isolates per-process regardless; this keeps plain `cargo test` solid.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const TARGET: TargetId = 7;

#[tokio::test]
async fn start_streams_frames_acks_each_then_stop_encodes() {
    let _env = ENV_LOCK.lock().await;
    let cdp = TestCdp::default();
    let rec = recorder(cdp.clone(), Ok(b"WEBM-OK".to_vec()));

    rec.start(TARGET, Caps::default()).await.expect("start ok");
    assert!(rec.is_recording(TARGET));
    assert_eq!(cdp.command_count("Page.startScreencast"), 1);

    cdp.fire_frame(b"frame-a", 1);
    cdp.fire_frame(b"frame-b", 2);
    cdp.fire_frame(b"frame-c", 3);
    // Let the ack task drain the queue (polls every 20ms).
    tokio::time::sleep(Duration::from_millis(80)).await;

    let outcome = rec.stop(TARGET).await;
    assert_eq!(outcome.frame_count, 3, "all frames buffered");
    assert_eq!(outcome.webm_bytes, b"WEBM-OK".to_vec());
    assert_eq!(outcome.stop_reason, "explicit");
    assert!(outcome.error.is_none());
    // Each frame must be acked (CDP backpressure), plus the stop command.
    assert_eq!(cdp.command_count("Page.screencastFrameAck"), 3);
    assert!(cdp.command_count("Page.stopScreencast") >= 1);
    assert!(!rec.is_recording(TARGET), "recording cleared after stop");
}

#[tokio::test]
async fn byte_cap_drops_over_cap_frames_and_reports_reason() {
    let _env = ENV_LOCK.lock().await;
    let cdp = TestCdp::default();
    let rec = recorder(cdp.clone(), Ok(b"X".to_vec()));
    // Cap that admits the first ~10-byte frame but not the second.
    let caps = Caps {
        max_duration_ms: 0,
        max_bytes: 8,
        frame_rate: 10,
    };
    rec.start(TARGET, caps).await.expect("start ok");
    cdp.fire_frame(b"0123456", 1); // 7 bytes — admitted
    cdp.fire_frame(b"0123456", 2); // would exceed 8 — dropped
    tokio::time::sleep(Duration::from_millis(60)).await;
    let outcome = rec.stop(TARGET).await;
    assert_eq!(outcome.frame_count, 1, "second frame dropped at byte cap");
    assert_eq!(outcome.stop_reason, "byte_cap");
    // Both frames still acked (we never stall the stream).
    assert_eq!(cdp.command_count("Page.screencastFrameAck"), 2);
}

/// Regression guard (RED-first) for the encode-failure-masks-cap bug: when a cap
/// was hit shim-side but the encode LATER fails (e.g. a flaky ffmpeg download),
/// the outcome MUST still report the TRUE `stop_reason` ("byte_cap"). The encode
/// failure belongs in `error` + empty `webm_bytes`, never by overwriting the
/// already-resolved stop reason. Fails before the fix (returns
/// "encoder_unavailable"); passes after.
#[tokio::test]
async fn byte_cap_reason_survives_encoder_failure() {
    let _env = ENV_LOCK.lock().await;
    let cdp = TestCdp::default();
    // Encoder FAILS (mimics ffmpeg unavailable) AND a byte cap is hit.
    let rec = recorder(cdp.clone(), Err("ffmpeg unavailable".to_string()));
    let caps = Caps {
        max_duration_ms: 0,
        max_bytes: 8,
        frame_rate: 10,
    };
    rec.start(TARGET, caps).await.expect("start ok");
    cdp.fire_frame(b"0123456", 1); // 7 bytes — admitted
    cdp.fire_frame(b"0123456", 2); // would exceed 8 — dropped → byte_cap
    tokio::time::sleep(Duration::from_millis(60)).await;
    let outcome = rec.stop(TARGET).await;
    assert_eq!(outcome.frame_count, 1, "second frame dropped at byte cap");
    assert_eq!(
        outcome.stop_reason, "byte_cap",
        "the true stop reason must survive an encode failure"
    );
    // Negative guard: the retired "encoder_unavailable" value must never resurface
    // as a stop_reason (the encode failure belongs in `error`, below).
    assert_ne!(outcome.stop_reason, "encoder_unavailable");
    assert_eq!(outcome.error.as_deref(), Some("ffmpeg unavailable"));
    assert!(outcome.webm_bytes.is_empty());
}

#[tokio::test]
async fn stop_without_start_is_a_clean_error() {
    let rec = recorder(TestCdp::default(), Ok(vec![]));
    let outcome = rec.stop(TARGET).await;
    assert_eq!(outcome.stop_reason, "error");
    assert!(outcome
        .error
        .as_deref()
        .unwrap()
        .contains("no active recording"));
    assert!(outcome.webm_bytes.is_empty());
}

#[tokio::test]
async fn double_start_is_rejected() {
    let _env = ENV_LOCK.lock().await;
    let rec = recorder(TestCdp::default(), Ok(vec![]));
    rec.start(TARGET, Caps::default()).await.expect("first ok");
    let err = rec.start(TARGET, Caps::default()).await.unwrap_err();
    assert!(err.contains("already active"));
    let _ = rec.stop(TARGET).await;
}

#[tokio::test]
async fn zero_frames_reports_no_frames() {
    let _env = ENV_LOCK.lock().await;
    let rec = recorder(TestCdp::default(), Ok(b"unused".to_vec()));
    rec.start(TARGET, Caps::default()).await.expect("start ok");
    let outcome = rec.stop(TARGET).await;
    assert_eq!(outcome.frame_count, 0);
    assert_eq!(outcome.stop_reason, "no_frames");
    assert!(outcome.error.is_some());
    assert!(outcome.webm_bytes.is_empty());
}

#[tokio::test]
async fn encoder_failure_is_best_effort() {
    let _env = ENV_LOCK.lock().await;
    let cdp = TestCdp::default();
    let rec = recorder(cdp.clone(), Err("ffmpeg unavailable".to_string()));
    rec.start(TARGET, Caps::default()).await.expect("start ok");
    cdp.fire_frame(b"frame", 1);
    tokio::time::sleep(Duration::from_millis(60)).await;
    let outcome = rec.stop(TARGET).await;
    assert_eq!(outcome.frame_count, 1);
    // No cap was hit, so the true stop reason is "explicit" — an encode failure
    // does NOT overwrite it (it surfaces via `error` + empty `webm_bytes`). The
    // retired "encoder_unavailable" value must never appear as a stop_reason.
    assert_eq!(outcome.stop_reason, "explicit");
    assert_ne!(outcome.stop_reason, "encoder_unavailable");
    assert_eq!(outcome.error.as_deref(), Some("ffmpeg unavailable"));
    assert!(outcome.webm_bytes.is_empty());
}

/// The duration cap auto-stops the screencast and reports `duration_cap` — and that
/// reason is reported correctly with a WORKING encoder (encoder-independent: the
/// cap is enforced shim-side in the ack task, before the encode). Concrete margined
/// timing: a 20ms cap vs a 200ms wait (the ack task polls every 20ms → ~10× margin),
/// so the cap check fires well before `stop()` is called.
#[tokio::test]
async fn duration_cap_auto_stops_and_reports_reason() {
    let _env = ENV_LOCK.lock().await;
    let cdp = TestCdp::default();
    let rec = recorder(cdp.clone(), Ok(b"WEBM-OK".to_vec()));
    let caps = Caps {
        max_duration_ms: 20,
        max_bytes: 0,
        frame_rate: 10,
    };
    rec.start(TARGET, caps).await.expect("start ok");
    cdp.fire_frame(b"frame", 1);
    // Wait well past the 20ms cap so the ack task hits the duration check and
    // auto-stops the screencast before we call stop().
    tokio::time::sleep(Duration::from_millis(200)).await;
    let outcome = rec.stop(TARGET).await;
    assert_eq!(outcome.stop_reason, "duration_cap");
    assert!(outcome.error.is_none());
    // The recorder auto-issued a stopScreencast when the cap fired.
    assert!(cdp.command_count("Page.stopScreencast") >= 1);
}

#[tokio::test]
async fn kill_switch_blocks_start() {
    let _env = ENV_LOCK.lock().await;
    // Tests run single-threaded (CLAUDE.md), so mutating env here is safe.
    std::env::set_var("LOOM_DISABLE_RECORDING", "1");
    let rec = recorder(TestCdp::default(), Ok(vec![]));
    let err = rec.start(TARGET, Caps::default()).await.unwrap_err();
    assert!(err.contains("LOOM_DISABLE_RECORDING"));
    std::env::remove_var("LOOM_DISABLE_RECORDING");
}

#[test]
fn caps_from_overrides_uses_defaults_and_clamps() {
    let d = Caps::from_overrides(None, None, None);
    assert_eq!(d.max_duration_ms, 300_000);
    assert_eq!(d.max_bytes, 268_435_456);
    assert_eq!(d.frame_rate, 10);

    let o = Caps::from_overrides(Some(1000), Some(42), Some(999));
    assert_eq!(o.max_duration_ms, 1000);
    assert_eq!(o.max_bytes, 42);
    assert_eq!(o.frame_rate, 60, "frame_rate clamps to <=60");

    // A 0 cap value maps to the safe DEFAULT (not "unlimited" / not 1) — the
    // public API must not be able to disable a cap by passing 0.
    let zeroed = Caps::from_overrides(Some(0), Some(0), Some(0));
    assert_eq!(zeroed.max_duration_ms, 300_000);
    assert_eq!(zeroed.max_bytes, 268_435_456);
    assert_eq!(zeroed.frame_rate, 10);
}

#[test]
fn ffmpeg_encoder_type_is_constructible() {
    // Compile-time guard that the production encoder implements the trait
    // (its actual encode path is covered by the #[ignore]d real e2e).
    let _e: Box<dyn ScreencastEncoder> = Box::new(FfmpegSidecarEncoder);
}
