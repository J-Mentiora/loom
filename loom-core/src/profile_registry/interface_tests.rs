//! Interface tests for `profile_registry`. Round-trip every canonical
//! constant member; assert garbage rejection.

use super::profile_registry::{
    is_known_budget_key, is_known_network_mode, is_known_profile, resolve_profile_alias,
    DEFAULT_PROFILE, KNOWN_BUDGET_KEYS, KNOWN_NETWORK_MODES, KNOWN_PROFILES,
};

#[test]
fn known_profiles_canonical_set() {
    assert_eq!(KNOWN_PROFILES, &["safe", "standard", "full"]);
}

#[test]
fn known_network_modes_canonical_set() {
    assert_eq!(KNOWN_NETWORK_MODES, &["live", "recorded", "mixed"]);
}

#[test]
fn known_budget_keys_canonical_set() {
    assert_eq!(
        KNOWN_BUDGET_KEYS,
        &["network", "wall_clock", "dom_nodes", "js_heap"]
    );
}

#[test]
fn is_known_profile_accepts_every_canonical_member() {
    for &p in KNOWN_PROFILES {
        assert!(is_known_profile(p), "{p} should be known");
    }
}

#[test]
fn is_known_profile_rejects_garbage() {
    assert!(!is_known_profile("nonexistent"));
    assert!(!is_known_profile(""));
    assert!(!is_known_profile("SAFE")); // case-sensitive
}

/// the SDKs send `profile: "default"` when the caller
/// doesn't choose one; it must resolve to the server default ("safe" —
/// identical to omitting the field) instead of `unknown_profile`.
#[test]
fn resolve_profile_alias_maps_default_to_server_default() {
    assert_eq!(resolve_profile_alias("default"), DEFAULT_PROFILE);
    assert_eq!(resolve_profile_alias("default"), "safe");
}

#[test]
fn resolve_profile_alias_passes_canonical_members_through() {
    for &p in KNOWN_PROFILES {
        assert_eq!(resolve_profile_alias(p), p, "{p} must pass through");
    }
}

#[test]
fn resolve_profile_alias_passes_unknown_through_for_downstream_rejection() {
    assert_eq!(resolve_profile_alias("nonexistent"), "nonexistent");
    assert_eq!(resolve_profile_alias(""), "");
    assert_eq!(resolve_profile_alias("DEFAULT"), "DEFAULT"); // case-sensitive
}

#[test]
fn default_profile_is_a_known_profile() {
    assert!(is_known_profile(DEFAULT_PROFILE));
}

#[test]
fn is_known_network_mode_accepts_every_canonical_member() {
    for &m in KNOWN_NETWORK_MODES {
        assert!(is_known_network_mode(m), "{m} should be known");
    }
}

#[test]
fn is_known_network_mode_rejects_garbage() {
    assert!(!is_known_network_mode("bogus"));
    assert!(!is_known_network_mode("offline")); // not in canonical set
    assert!(!is_known_network_mode(""));
}

#[test]
fn is_known_budget_key_accepts_every_canonical_member() {
    for &k in KNOWN_BUDGET_KEYS {
        assert!(is_known_budget_key(k), "{k} should be known");
    }
}

#[test]
fn is_known_budget_key_rejects_garbage() {
    assert!(!is_known_budget_key("garbage"));
    assert!(!is_known_budget_key("Network")); // case-sensitive
    assert!(!is_known_budget_key(""));
}
