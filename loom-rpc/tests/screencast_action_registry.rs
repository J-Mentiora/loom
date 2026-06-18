//! TDD RED acceptance contract for the video / screen-capture feature
//! (spec: specs/2026-06-18-video-capture/plan.md).
//!
//! These assert the NEW public action-registry contract: the bracketed
//! `web.start_recording` / `web.stop_recording` verbs and their params. They
//! FAIL today (the verbs don't exist yet) and turn GREEN once Phase 3 lands the
//! registry + router + shim screencast recorder. The full behavioural suite
//! (fake-chromium screencast e2e, the determinism / replay-equality test proving
//! the `.webm` is excluded from the manifest hash chain, capture-policy
//! downgrade, caps auto-stop, and the encoder-absent best-effort path) is
//! enumerated in specs/2026-06-18-video-capture/agent-test-spec.md and built in
//! Phase 3.

use loom_rpc::action_registry::{find, ParamMeta};

fn has_param(verb: &str, param: &str) -> bool {
    find(verb)
        .unwrap_or_else(|| panic!("{verb} must be a registered action"))
        .params
        .iter()
        .any(|p: &ParamMeta| p.name == param)
}

#[test]
fn start_recording_verb_is_registered() {
    assert!(
        find("web.start_recording").is_some(),
        "video-capture: `web.start_recording` must be registered in the action registry"
    );
    assert!(
        has_param("web.start_recording", "session_id"),
        "web.start_recording must take `session_id`"
    );
    // Resource caps are overridable per-recording (decisions.md D5): the
    // duration cap is the load-bearing safety knob, so it must be exposed.
    assert!(
        has_param("web.start_recording", "max_duration_ms"),
        "web.start_recording must expose the `max_duration_ms` cap override"
    );
}

#[test]
fn stop_recording_verb_is_registered() {
    assert!(
        find("web.stop_recording").is_some(),
        "video-capture: `web.stop_recording` must be registered in the action registry"
    );
    assert!(
        has_param("web.stop_recording", "session_id"),
        "web.stop_recording must take `session_id`"
    );
}
