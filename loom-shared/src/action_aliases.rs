//! `action_aliases` — single source of truth for JSON-RPC method-name aliases.
//!
//! A canonical method name is the one stored in `BUILTIN_SCHEMAS` (CLI side)
//! and the one returned by `rpc.schemas` as `MethodSchema::method`. Aliases
//! are alternate spellings accepted on input by both layers and rewritten to
//! the canonical name before any further processing (validation, dispatch,
//! observability).
//!
//! Each entry is `(alias, canonical)`. The `alias` is what a client may send;
//! the `canonical` is what flows downstream. The literal kebab spelling
//! `web.type-text` is intentionally absent — it is rejected with a hint that
//! lists `web.type` and `web.type_text` from the alias-aware `methods()`
//! enumeration .
//!
//! ## Adding a new alias
//!
//! 1. Append a row to `METHOD_ALIASES`.
//! 2. Confirm the `canonical` half exists in
//!    `loom-cli/src/postinstall_runner/interfaces.rs::BUILTIN_SCHEMAS`.
//!    The `every_alias_target_appears_in_builtin_schemas` integration test
//!    will fail the build otherwise.

/// `(alias, canonical)`. Order is stable; consumers preserve order in
/// `aliases_of` for deterministic JSON output (`rpc.schemas`).
///
/// `action.web.*` aliases shipped with the @mentiora-ai/loom-sdk@0.9.0
/// generation, which prefixes every action verb with the surface type.
/// The router-canonical form is the unprefixed `web.*` shape.
pub const METHOD_ALIASES: &[(&str, &str)] = &[
    ("web.type_text", "web.type"),
    ("action.web.navigate", "web.navigate"),
    ("action.web.click", "web.click"),
    ("action.web.type", "web.type"),
    ("action.web.type_text", "web.type"),
    ("action.web.select", "web.select"),
    ("action.web.hover", "web.hover"),
    ("action.web.scroll", "web.scroll"),
    ("action.web.wait", "web.wait"),
    ("action.web.wait_for", "web.wait_for"),
    ("action.web.evaluate", "web.evaluate"),
    ("action.web.screenshot", "web.screenshot"),
    ("action.web.snapshot", "web.snapshot"),
    // video-capture: both SDKs spell these `action.web.start_recording` /
    // `action.web.stop_recording` (python `Session.start_recording`, typescript
    // `Session.startRecording`). A missing row here = method_not_found on every
    // real-daemon call (the canonicaliser is table-only — no generic strip).
    ("action.web.start_recording", "web.start_recording"),
    ("action.web.stop_recording", "web.stop_recording"),
    // voice-call-io: the 4 audio verbs. Each canonical target has a
    // `BUILTIN_SCHEMAS` entry (enforced by `test_every_alias_target_appears_in_builtin_schemas`).
    ("action.web.inject_audio", "web.inject_audio"),
    ("action.web.start_audio_capture", "web.start_audio_capture"),
    ("action.web.stop_audio_capture", "web.stop_audio_capture"),
    ("action.web.say", "web.say"),
];

/// Resolve `method` to its canonical name. Unknown or already-canonical
/// names pass through unchanged.
pub fn canonicalise(method: &str) -> &str {
    for (alias, canonical) in METHOD_ALIASES {
        if *alias == method {
            return canonical;
        }
    }
    method
}

/// All alias names that resolve to `canonical`. Returned in
/// `METHOD_ALIASES` order so callers (e.g. `rpc.schemas` snapshot
/// builders) get deterministic output.
pub fn aliases_of(canonical: &str) -> Vec<&'static str> {
    METHOD_ALIASES
        .iter()
        .filter_map(|(alias, c)| if *c == canonical { Some(*alias) } else { None })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalise_returns_input_unchanged_for_canonical() {
        assert_eq!(canonicalise("web.type"), "web.type");
        assert_eq!(canonicalise("web.click"), "web.click");
    }

    #[test]
    fn canonicalise_returns_input_unchanged_for_unknown() {
        assert_eq!(canonicalise("session.create"), "session.create");
        assert_eq!(canonicalise("bogus.method"), "bogus.method");
    }

    #[test]
    fn canonicalise_resolves_known_alias() {
        assert_eq!(canonicalise("web.type_text"), "web.type");
    }

    /// Both SDKs spell the settle-capture verb
    /// `action.web.wait_for` (python `Session.wait_for`, typescript
    /// `Session.waitFor`); a missing row here surfaced as
    /// `method_not_found` on every real-daemon call.
    #[test]
    fn canonicalise_resolves_sdk_wait_for_alias() {
        assert_eq!(canonicalise("action.web.wait_for"), "web.wait_for");
    }

    #[test]
    fn canonicalise_does_not_resolve_kebab_literal() {
        // `web.type-text` (literal kebab) is NOT an alias —
        // it must surface as unknown so the CLI/RPC error path fires.
        assert_eq!(canonicalise("web.type-text"), "web.type-text");
    }

    #[test]
    fn aliases_of_returns_empty_for_unknown_canonical() {
        // session.* is not aliased.
        assert!(aliases_of("session.create").is_empty());
        // bogus name has no aliases.
        assert!(aliases_of("bogus.method").is_empty());
    }

    /// `web.click` has the SDK-envelope alias `action.web.click` but
    /// no other alias. Order is stable per METHOD_ALIASES.
    #[test]
    fn aliases_of_returns_envelope_alias_for_web_click() {
        assert_eq!(aliases_of("web.click"), vec!["action.web.click"]);
    }

    /// `web.type` has the historical kebab-friendly alias plus both
    /// SDK-envelope variants — the `.type` and the legacy `.type_text`
    /// the SDK ships for backward compat.
    #[test]
    fn aliases_of_returns_aliases_for_web_type() {
        assert_eq!(
            aliases_of("web.type"),
            vec!["web.type_text", "action.web.type", "action.web.type_text"]
        );
    }

    /// Every `action.web.*` alias resolves to the same `web.*` canonical
    /// name as its unprefixed cousin — guards against typos like
    /// `action.web.navigate` accidentally pointing at `web.click`.
    #[test]
    fn every_action_web_alias_resolves_to_matching_web_canonical() {
        for (alias, canonical) in METHOD_ALIASES {
            if let Some(stripped) = alias.strip_prefix("action.") {
                // `action.web.type_text` → `web.type` (collapses through the
                // existing `web.type_text` alias). Skip that intentional
                // many-to-one case and check the straight pass-through ones.
                if *alias == "action.web.type_text" {
                    assert_eq!(*canonical, "web.type");
                    continue;
                }
                assert_eq!(stripped, *canonical, "alias {alias} → {canonical} mismatch");
            }
        }
    }
}
