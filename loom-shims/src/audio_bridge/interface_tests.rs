//! `AudioBridge::inject` tests — drive the inject flow against a `TestCdp` +
//! `FakeTargets` double (no real Chromium). These prove the CDP MECHANICS:
//! that inject resolves the api objectId then calls `enqueue` with the audio
//! bytes as a **CallArgument** (never string-interpolated, Architecture A3),
//! threads `await_playout`, and maps page rejections to typed errors. The
//! audio-actually-reaches-the-track proof is the `#[ignore]`d real-browser
//! check in `loom-cli/tests/live_voice_e2e.rs`; the full round-trip is task 07.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use ciborium::value::Value as CborValue;
use parking_lot::Mutex;

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
    /// What `Runtime.callFunctionOn` returns (default: `{result:{value:0.8}}`).
    callfn_response: Arc<Mutex<CborValue>>,
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
            "Runtime.callFunctionOn" => Ok(self.callfn_response.lock().clone()),
            "Runtime.releaseObject" => Ok(CborValue::Null),
            other => panic!("unexpected CDP method issued by inject: {other}"),
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
