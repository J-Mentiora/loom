//! Surface contract for the voice-call-io audio verbs
//! (XL plan: voice-call-io, task 02 — "Wire + surface", AC8).
//!
//! Asserts the daemon/MCP-facing surface of `web.inject_audio`,
//! `web.start_audio_capture`, `web.stop_audio_capture`, and `web.say`: each is
//! registered, exposes the right params, has registry↔router required-param
//! parity, appears in `known_router_methods`, has a `BUILTIN_SCHEMAS` entry
//! (what surfaces in MCP `tools/list`), and has an `action.web.*` alias that
//! canonicalises back to it. This is SURFACE only — no dispatch behavior is
//! exercised (the daemon/shim wiring lands in later tasks). The generic
//! registry-wide parity/lint suite lives in
//! `loom-rpc/src/action_registry/interface_tests.rs`; this file is the
//! feature-specific contract that a future refactor of these four verbs must
//! keep green.

use loom_rpc::action_registry::{find, ParamMeta};
use loom_rpc::request_router::{known_router_methods, router_required_params};
use loom_shared::action_aliases::canonicalise;
use loom_shared::builtin_schemas::BUILTIN_SCHEMAS;

const AUDIO_VERBS: &[&str] = &[
    "web.inject_audio",
    "web.start_audio_capture",
    "web.stop_audio_capture",
    "web.say",
];

fn required_params(verb: &str) -> Vec<&'static str> {
    let mut names: Vec<&'static str> = find(verb)
        .unwrap_or_else(|| panic!("{verb} must be a registered action"))
        .params
        .iter()
        .filter(|p: &&ParamMeta| p.required)
        .map(|p| p.name)
        .collect();
    names.sort_unstable();
    names
}

fn has_param(verb: &str, param: &str) -> bool {
    find(verb)
        .unwrap_or_else(|| panic!("{verb} must be a registered action"))
        .params
        .iter()
        .any(|p: &ParamMeta| p.name == param)
}

/// Every audio verb is registered and takes `session_id`.
#[test]
fn audio_verbs_are_registered_with_session_id() {
    for verb in AUDIO_VERBS {
        assert!(
            find(verb).is_some(),
            "voice-call-io: `{verb}` must be registered in the action registry"
        );
        assert!(
            has_param(verb, "session_id"),
            "`{verb}` must take `session_id`"
        );
    }
}

/// Registry required-params exactly match the router's for each audio verb.
/// This is the verb-scoped view of `registry_required_flags_match_router`;
/// duplicating it here makes the audio-surface contract self-contained.
#[test]
fn audio_verbs_registry_router_required_parity() {
    for verb in AUDIO_VERBS {
        let registry = required_params(verb);
        let mut router: Vec<&'static str> = router_required_params(verb)
            .unwrap_or_else(|| panic!("`{verb}` must have a router_required_params row"))
            .to_vec();
        router.sort_unstable();
        assert_eq!(
            registry, router,
            "`{verb}` registry required-params {registry:?} must equal router required-params {router:?}"
        );
    }
}

/// `web.say` additionally requires `text`; the capture/inject verbs require only
/// `session_id`. Guards against a required-param drift the parity test alone
/// (which only checks the two sides agree) would not localize.
#[test]
fn audio_verb_required_param_shapes() {
    assert_eq!(required_params("web.inject_audio"), vec!["session_id"]);
    assert_eq!(
        required_params("web.start_audio_capture"),
        vec!["session_id"]
    );
    assert_eq!(
        required_params("web.stop_audio_capture"),
        vec!["session_id"]
    );
    let mut say = required_params("web.say");
    say.sort_unstable();
    assert_eq!(
        say,
        vec!["session_id", "text"],
        "`web.say` must require `text`"
    );
    assert!(
        has_param("web.say", "text"),
        "`web.say` must expose the `text` param"
    );
}

/// Every audio verb is in the router's known-method set (routable).
#[test]
fn audio_verbs_are_known_router_methods() {
    let known = known_router_methods();
    for verb in AUDIO_VERBS {
        assert!(
            known.contains(verb),
            "`{verb}` must be listed in known_router_methods()"
        );
    }
}

/// Every audio verb has a `BUILTIN_SCHEMAS` entry (what surfaces in MCP
/// `tools/list`), and the request schema references the wire `session` field.
#[test]
fn audio_verbs_have_builtin_schemas() {
    for verb in AUDIO_VERBS {
        let entry = BUILTIN_SCHEMAS.iter().find(|(name, _)| name == verb);
        let (_, schema) =
            entry.unwrap_or_else(|| panic!("`{verb}` must have a BUILTIN_SCHEMAS entry"));
        assert!(
            schema.contains("\"session\""),
            "`{verb}` schema must accept the wire `session` field"
        );
    }
}

/// `web.say`'s schema declares `text` as a required request field.
#[test]
fn say_schema_requires_text() {
    let (_, schema) = BUILTIN_SCHEMAS
        .iter()
        .find(|(name, _)| *name == "web.say")
        .expect("web.say must have a BUILTIN_SCHEMAS entry");
    assert!(
        schema.contains("\"required\":[\"session\",\"text\"]"),
        "web.say schema must require both `session` and `text`"
    );
}

/// Each `action.web.<verb>` alias canonicalises back to its bare `web.<verb>`.
#[test]
fn audio_verb_aliases_canonicalise() {
    for verb in AUDIO_VERBS {
        let alias = format!("action.{verb}");
        assert_eq!(
            canonicalise(&alias),
            *verb,
            "`{alias}` must canonicalise to `{verb}`"
        );
    }
}
