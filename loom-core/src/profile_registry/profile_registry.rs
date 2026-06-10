//! `profile_registry` — canonical session profile / network-mode / budget
//! key allowlists.
//!
//! The single source of truth for the value sets accepted by
//! `session.create`. Used by `loom-rpc::session_validation` to reject
//! bogus inputs at the JSON-RPC boundary. Sets are sourced from
//! `foundation/glossary.md` L14-15 (profiles + network modes) and the
//! existing `parse_budget_string` arms (budget keys).

/// Canonical sandbox profile names. Glossary §Profile (Sandbox Profile).
pub const KNOWN_PROFILES: &[&str] = &["safe", "standard", "full"];

/// The server-default profile — the one applied when `session.create`
/// omits the `profile` field entirely (mirrors
/// `loom_rpc::core_service_adapter::CreateSessionParams::default_profile`
/// and the CLI's no-`--profile` behavior).
pub const DEFAULT_PROFILE: &str = "safe";

/// SDK wire alias for the server-default profile. Both SDKs send
/// `profile: "default"` when the caller doesn't pick one, so the
/// daemon must resolve it to [`DEFAULT_PROFILE`] rather than reject
/// it as `unknown_profile`.
pub const PROFILE_ALIAS_DEFAULT: &str = "default";

/// Resolve the SDK `"default"` profile alias to the canonical
/// server-default profile. Canonical and unknown names pass through
/// unchanged (unknowns are rejected downstream by the allowlist).
pub fn resolve_profile_alias(s: &str) -> &str {
    if s == PROFILE_ALIAS_DEFAULT {
        DEFAULT_PROFILE
    } else {
        s
    }
}

/// Canonical network-mode names. Glossary §Network Mode.
pub const KNOWN_NETWORK_MODES: &[&str] = &["live", "recorded", "mixed"];

/// Canonical budget keys. Mirrors `parse_budget_string` accepted keys.
pub const KNOWN_BUDGET_KEYS: &[&str] = &["network", "wall_clock", "dom_nodes", "js_heap"];

/// Returns true if `s` is a canonical profile name.
pub fn is_known_profile(s: &str) -> bool {
    KNOWN_PROFILES.contains(&s)
}

/// Returns true if `s` is a canonical network-mode name.
pub fn is_known_network_mode(s: &str) -> bool {
    KNOWN_NETWORK_MODES.contains(&s)
}

/// Returns true if `s` is a canonical budget key.
pub fn is_known_budget_key(s: &str) -> bool {
    KNOWN_BUDGET_KEYS.contains(&s)
}
