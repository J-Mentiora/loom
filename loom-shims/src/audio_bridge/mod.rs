// audio_bridge — synthetic-microphone injection + (task 05) capture, for
// in-browser WebRTC voice calls. Mirrors `screencast_recorder/` structure so the
// tests can mirror too.
//
// # Task 03 scope (this file)
// The mic-override HALF: nonce generation, rendering the `audio_bootstrap.js`
// asset with a fresh per-target nonce, and building the
// `Page.addScriptToEvaluateOnNewDocument` install params. `TargetManager::
// create_new_target` installs the rendered bootstrap for `--audio` sessions.
//
// # Deferred (later tasks — clearly-marked seams below)
// - task 04: `AudioBridge::inject` (payload → `callFunctionOn(enqueue)`).
// - task 05: capture tap / ring buffer / drain (`start_capture`/`stop_capture`).
//
// No WIT / no WASM guest change — audio is a daemon-side side-channel that rides
// the existing `shim_protocol` wire (`audio_enabled` on SpawnTarget/PageNavigate).

use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

// task 04: inject side (`AudioBridge::inject`). Split into its own file, mirroring
// `screencast_recorder/`, so its tests + the future capture impl live beside it.
mod audio_bridge;
pub use audio_bridge::{AudioBridge, Caps};

// task 05: capture pipeline building blocks (native → 16 kHz linear resampler and
// the hand-written mono i16 WAV mux). Used by `AudioBridge::stop_capture`.
mod resample;
mod wav;

#[cfg(test)]
mod interface_tests;

/// The raw, un-rendered mic-override bootstrap. Embedded at compile time; the
/// `__LOOM_AUDIO_NONCE__` token is substituted per target by
/// [`render_bootstrap_script`].
pub const AUDIO_BOOTSTRAP_TEMPLATE: &str = include_str!("../../assets/audio_bootstrap.js");

/// Template token replaced with the per-target nonce.
pub const NONCE_TOKEN: &str = "__LOOM_AUDIO_NONCE__";

/// CDP method used to install the bootstrap (parity with the determinism inject).
pub const ADD_SCRIPT_METHOD: &str = "Page.addScriptToEvaluateOnNewDocument";

/// `runImmediately: true` is load-bearing — without it the override applies only
/// from the SECOND document onward and the app would grab the real (fake-device)
/// mic on first load.
pub const RUN_IMMEDIATELY: bool = true;

/// Process-wide monotonic counter mixed into every nonce so two nonces minted in
/// the same nanosecond still differ (FND-0015 distinct-nonce-per-render).
static NONCE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a fresh short nonce (16 lowercase-hex chars) for a target. loom-shims
/// carries no `rand`/`uuid` dependency, so the nonce is derived by hashing a
/// process-monotonic counter + a high-resolution timestamp + a stack address
/// with SHA-256 and taking the first 8 bytes. The nonce is NOT a security
/// boundary (the main-world override is discoverable by design — FND-0019); it
/// exists for per-session API isolation + track provenance (task 05 capture
/// exclusion), which only require uniqueness, satisfied here.
pub fn generate_nonce() -> String {
    let counter = NONCE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let stack_marker = &counter as *const _ as usize;
    let seed = format!("{counter}:{nanos}:{stack_marker}");
    let digest = Sha256::digest(seed.as_bytes());
    let mut out = String::with_capacity(16);
    for byte in &digest[..8] {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Render the mic-override bootstrap with `nonce` substituted for
/// `__LOOM_AUDIO_NONCE__`. Every occurrence of the token is replaced, so the
/// exposed API becomes `window.__loom_<nonce>`.
pub fn render_bootstrap_script(nonce: &str) -> String {
    AUDIO_BOOTSTRAP_TEMPLATE.replace(NONCE_TOKEN, nonce)
}

/// Build the CBOR params for the `Page.addScriptToEvaluateOnNewDocument` command
/// that installs the rendered mic-override, given the per-target `nonce`.
/// Canonical shape `{"source": <rendered js>, "runImmediately": true}` — mirrors
/// `determinism_injector::build_inject_params`.
pub fn build_install_params(nonce: &str) -> ciborium::value::Value {
    use ciborium::value::Value;
    let source = render_bootstrap_script(nonce);
    Value::Map(vec![
        (Value::Text("source".into()), Value::Text(source)),
        (
            Value::Text("runImmediately".into()),
            Value::Bool(RUN_IMMEDIATELY),
        ),
    ])
}

// ── task 05 seam (Architecture §5) ───────────────────────────────────────────
//
// Task 04 landed `AudioBridge::inject` in `audio_bridge.rs`. The capture side
// (`start_capture`/`stop_capture` + a per-target `CaptureState` map, `Caps`,
// `AudioClock`) is task 05 and extends the same struct. It is intentionally NOT
// stubbed yet: premature `Caps`/`AudioClock` shapes would just be churn to reshape
// when that task lands.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_substitutes_nonce_and_leaves_no_token() {
        let nonce = "deadbeefcafe0001";
        let rendered = render_bootstrap_script(nonce);
        assert!(
            !rendered.contains(NONCE_TOKEN),
            "no residual __LOOM_AUDIO_NONCE__ token must remain"
        );
        assert!(
            rendered.contains(&format!("window.__loom_{nonce}")),
            "rendered API key must be window.__loom_<nonce>"
        );
    }

    #[test]
    fn distinct_nonces_per_render() {
        // FND-0015: two renders must produce DISTINCT nonces.
        let a = generate_nonce();
        let b = generate_nonce();
        assert_ne!(a, b, "nonces must be unique per generation");
        assert_eq!(a.len(), 16, "nonce is 16 hex chars");
        assert!(
            a.chars().all(|c| c.is_ascii_hexdigit()),
            "nonce must be lowercase hex, got {a}"
        );
    }

    #[test]
    fn install_params_have_source_and_run_immediately() {
        use ciborium::value::Value;
        let nonce = "0011223344556677";
        let params = build_install_params(nonce);
        let map = match &params {
            Value::Map(m) => m,
            other => panic!("expected Map params, got {other:?}"),
        };
        let source = map
            .iter()
            .find(|(k, _)| k == &Value::Text("source".into()))
            .and_then(|(_, v)| match v {
                Value::Text(s) => Some(s.clone()),
                _ => None,
            })
            .expect("params must carry a `source` string");
        assert!(source.contains(&format!("window.__loom_{nonce}")));
        assert!(!source.contains(NONCE_TOKEN));
        let run_immediately = map
            .iter()
            .find(|(k, _)| k == &Value::Text("runImmediately".into()))
            .map(|(_, v)| v == &Value::Bool(true))
            .unwrap_or(false);
        assert!(run_immediately, "runImmediately must be true (R3 parity)");
    }

    #[test]
    fn template_carries_leak_guard_and_non_enumerable_api() {
        // The task-03 half must: guard the audio+video real-mic leak (FND-0022),
        // expose the API non-enumerable (FND-0019), reject enqueue-before-gUM
        // (AC10), and leave the capture tap for task 05.
        assert!(AUDIO_BOOTSTRAP_TEMPLATE.contains(NONCE_TOKEN));
        assert!(AUDIO_BOOTSTRAP_TEMPLATE.contains("getVideoTracks"));
        assert!(AUDIO_BOOTSTRAP_TEMPLATE.contains("createMediaStreamDestination"));
        assert!(AUDIO_BOOTSTRAP_TEMPLATE.contains("enumerable: false"));
        assert!(AUDIO_BOOTSTRAP_TEMPLATE.contains("no_microphone_request"));
        assert!(AUDIO_BOOTSTRAP_TEMPLATE.contains("decodeAudioData"));
    }
}
