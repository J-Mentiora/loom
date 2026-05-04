// Safety — profile-based restrictions on web surface actions.
//
// # Contract semantics
// - **AC-WEB-07.1:** When `profile = Safe`, `check_evaluate` scans the
//   expression against `EVALUATE_DENYLIST` (substring match). A match
//   returns `Some(PolicyViolation::EvaluateDenylistMatch)`.
// - **AC-WEB-07.2:** `is_session_scoped_path` checks whether a download
//   target is under the session-scoped downloads directory. Used by the
//   host shim to enforce download path restrictions at the CDP level.
// - **AC-SAFETY-02.1:** `is_loom_data_path` checks that a candidate file
//   path is under the resolved loom data root (`~/.loom/` or
//   `$XDG_DATA_HOME/loom`). Used by host-side write guards.
// - **Pure (no I/O, no allocations beyond Option/String).**
// - **WASM-safe.** No `std::time`, `std::net`, `std::fs`, `getrandom`.
//
// # Banned in this module
// - `std::time`, `std::net`, `std::fs::write`, `getrandom`, `HashMap`.


extern crate alloc;

use alloc::string::String;

/// Session safety profile. Carried in every action dispatched to a web
/// surface verb. Set at session creation from `CreateSessionParams::profile`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafetyProfile {
    /// Default profile — no extra restrictions beyond budget + vault.
    Default,
    /// Safe profile — blocks destructive evaluate expressions and restricts
    /// download paths to the session-scoped downloads directory.
    Safe,
}

/// Patterns blocked by `SafetyPolicy::check_evaluate` when
/// `SafetyProfile::Safe` is active. Substring match against the full
/// expression string. (FR-WEB-07, FR-SAFETY-01)
///
/// Covers storage mutation and code injection vectors. Network APIs
/// (`fetch`, `XMLHttpRequest`) are intentionally omitted — they are
/// handled by the vault grant mechanism, not the evaluate denylist.
pub const EVALUATE_DENYLIST: &[&str] = &[
    "document.cookie",
    "localStorage",
    "sessionStorage",
    "indexedDB",
    "document.write(",
    "eval(",
    "Function(",
    // AC-SAFEPROF-01 additions (operator's regex patterns, translated to
    // substrings to avoid pulling in the `regex` crate).
    //
    // `window.location` — broader than the regex `window\.location[ \t]*=`;
    // catches reads (`console.log(window.location)`) too. Acceptable for
    // safe profile as defense-in-depth — operator opted into restrictions.
    "window.location",
    // `navigator.serviceWorker.register` — tightened from the regex
    // `navigator\.serviceWorker` so feature-detect via
    // `if (navigator.serviceWorker) { ... }` still works. The destructive
    // op is `.register()`; reading the property is not.
    "navigator.serviceWorker.register",
];

/// Violation returned when a safety policy check fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyViolation {
    /// AC-WEB-07.1: expression matched a pattern in `EVALUATE_DENYLIST`.
    EvaluateDenylistMatch,
    /// AC-WEB-07.2: download target path is outside the session-scoped dir.
    DownloadPathBlocked {
        candidate_path: String,
    },
}

/// Stateless safety policy checker. All functions are pure — no I/O, no
/// allocations beyond the optional return value.
pub struct SafetyPolicy;

impl SafetyPolicy {
    /// Check whether `expression` is permitted under `profile`.
    ///
    /// Returns `Some(PolicyViolation::EvaluateDenylistMatch)` when
    /// `profile == Safe` and the expression contains any pattern from
    /// `EVALUATE_DENYLIST`. Returns `None` when the expression is allowed.
    ///
    /// Used by `EvaluateVerb::execute` as a pre-CDP gate (AC-WEB-07.1).
    pub fn check_evaluate(profile: SafetyProfile, expression: &str) -> Option<PolicyViolation> {
        if profile != SafetyProfile::Safe {
            return None;
        }
        for pattern in EVALUATE_DENYLIST {
            if expression.contains(pattern) {
                return Some(PolicyViolation::EvaluateDenylistMatch);
            }
        }
        None
    }

    /// Check whether `path` is inside `session_downloads_dir`.
    ///
    /// Returns `true` when `path` starts with `session_downloads_dir/`
    /// (trailing slash normalised). Used by the host shim to validate
    /// download targets under `SafetyProfile::Safe` (AC-WEB-07.2).
    pub fn is_session_scoped_path(path: &str, session_downloads_dir: &str) -> bool {
        let base = if session_downloads_dir.ends_with('/') {
            alloc::borrow::Cow::Borrowed(session_downloads_dir)
        } else {
            alloc::borrow::Cow::Owned(alloc::format!("{}/", session_downloads_dir))
        };
        path.starts_with(base.as_ref())
    }

    /// Check whether `path` is under `loom_data_root`.
    ///
    /// Returns `true` when `path` starts with `loom_data_root/`. Used by
    /// host-side write guards to enforce AC-SAFETY-02.1 (all session data
    /// under `~/.loom/` or `$XDG_DATA_HOME/loom`).
    pub fn is_loom_data_path(path: &str, loom_data_root: &str) -> bool {
        let base = if loom_data_root.ends_with('/') {
            alloc::borrow::Cow::Borrowed(loom_data_root)
        } else {
            alloc::borrow::Cow::Owned(alloc::format!("{}/", loom_data_root))
        };
        path.starts_with(base.as_ref())
    }
}
