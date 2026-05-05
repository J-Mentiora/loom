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
pub const METHOD_ALIASES: &[(&str, &str)] = &[("web.type_text", "web.type")];

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

    #[test]
    fn canonicalise_does_not_resolve_kebab_literal() {
        // `web.type-text` (literal kebab) is NOT an alias —
        // it must surface as unknown so the CLI/RPC error path fires.
        assert_eq!(canonicalise("web.type-text"), "web.type-text");
    }

    #[test]
    fn aliases_of_returns_empty_for_unknown_canonical() {
        assert!(aliases_of("session.create").is_empty());
        assert!(aliases_of("web.click").is_empty());
    }

    #[test]
    fn aliases_of_returns_alias_for_web_type() {
        assert_eq!(aliases_of("web.type"), vec!["web.type_text"]);
    }
}
