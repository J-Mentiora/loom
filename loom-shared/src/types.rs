//! Shared domain newtypes that ride the host↔shim wire.
//!
//! Both `Seed` and `EpochMs` use `#[serde(transparent)]` so the CBOR
//! encoding is bitwise-identical to a bare `u64` — no wire-format change.
//! The compile-time benefit is preventing transposition: a `Seed` cannot
//! be passed where an `EpochMs` is expected (or vice-versa), and neither
//! can be confused with the existing `SessionId`/`TargetId` u64 aliases.

use serde::{Deserialize, Serialize};

/// Session-determinism seed. `Seed(0)` is a real value distinct from
/// "absent"; callers MUST NOT use 0 as a sentinel. The `Option<u64> →
/// Seed` collapse happens exactly once, at `LocalSessionManager::create`,
/// against `config.default_seed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Seed(pub u64);

/// Session-fixed Unix epoch milliseconds for `Date.now()` substitution
/// inside the determinism JS template.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EpochMs(pub u64);

/// Validate a session_id string for path-safety. Returns true iff the
/// string is exactly 26 characters of lowercase ASCII alphanumeric
/// (the ULID format used by `LocalSessionManager::create`).
///
/// Why: callers like `core_api_facade::inspect_session_json` build
/// filesystem paths via `sessions_root.join(session_id).join("manifest.wal")`.
/// Without this gate, a malicious session_id like `../../etc/passwd`
/// resolves to a file outside `sessions_root`, leaking arbitrary
/// readable file content as receipt entries.
pub fn is_valid_session_id(s: &str) -> bool {
    s.len() == 26
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shim_protocol::ciborium_to_vec;

    #[test]
    fn seed_wire_encoding_is_bare_u64() {
        let s = Seed(42);
        let s_bytes = ciborium_to_vec(&s).unwrap();
        let raw_bytes = ciborium_to_vec(&42u64).unwrap();
        assert_eq!(s_bytes, raw_bytes);
    }

    #[test]
    fn epoch_ms_wire_encoding_is_bare_u64() {
        let e = EpochMs(1_700_000_000_000);
        let e_bytes = ciborium_to_vec(&e).unwrap();
        let raw_bytes = ciborium_to_vec(&1_700_000_000_000u64).unwrap();
        assert_eq!(e_bytes, raw_bytes);
    }

    #[test]
    fn seed_zero_round_trips() {
        let s = Seed(0);
        let bytes = ciborium_to_vec(&s).unwrap();
        let back: Seed = ciborium::de::from_reader(&bytes[..]).unwrap();
        assert_eq!(s, back);
    }
}

#[cfg(test)]
mod session_id_tests {
    use super::is_valid_session_id;

    #[test]
    fn accepts_real_ulid_lowercase() {
        assert!(is_valid_session_id("01kqrr5g051wysed5j0kf6g9yw"));
        assert!(is_valid_session_id("01kqd56na91x444yvnb0gqhdje"));
    }

    #[test]
    fn rejects_path_traversal() {
        assert!(!is_valid_session_id("../etc/passwd"));
        assert!(!is_valid_session_id("../../../../tmp/evil"));
        assert!(!is_valid_session_id(".."));
    }

    #[test]
    fn rejects_directory_separators() {
        assert!(!is_valid_session_id("a/b/c"));
        assert!(!is_valid_session_id("01kqd56na91x444yvnb0gqhd/e"));
    }

    #[test]
    fn rejects_wrong_length() {
        assert!(!is_valid_session_id(""));
        assert!(!is_valid_session_id("too-short"));
        assert!(!is_valid_session_id("0123456789012345678901234567")); // 28
    }

    #[test]
    fn rejects_uppercase() {
        assert!(!is_valid_session_id("01KQD56NA91X444YVNB0GQHDJE"));
    }

    #[test]
    fn rejects_non_alphanumeric() {
        assert!(!is_valid_session_id("01kqd56na91x444yvnb0gqh!je"));
        assert!(!is_valid_session_id("01kqd56na91x444yvnb0gqhdj_"));
    }
}
