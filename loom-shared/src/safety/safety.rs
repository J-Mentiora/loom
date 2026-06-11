// Safety — profile-based restrictions on web surface actions.
//
// # Contract semantics
// - **Evaluate denylist:** When `profile = Safe`, `check_evaluate` scans
//   the expression against `EVALUATE_DENYLIST` via `find_denylist_match`
//   (substring match on the raw expression AND on a comment/whitespace-
//   stripped form). A match returns
//   `Some(PolicyViolation::EvaluateDenylistMatch)`. See the threat-model
//   note on `EVALUATE_DENYLIST` — this is a guardrail, not a sandbox.
// - **Download path scoping:** `is_session_scoped_path` checks whether a
//   download target is under the session-scoped downloads directory. Used
//   by the host shim to enforce download path restrictions at the CDP level.
// - **Loom data root:** `is_loom_data_path` checks that a candidate file
//   path is under the resolved loom data root (`~/.loom/` or
//   `$XDG_DATA_HOME/loom`). Used by host-side write guards.
// - **Path checks are lexical:** `.` / `..` components are resolved
//   without filesystem access (this module is pure + WASM-safe), so
//   symlinks are NOT chased. Callers that need symlink-safe containment
//   must canonicalize via the filesystem before calling.
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
/// `SafetyProfile::Safe` is active. Matched via [`find_denylist_match`]:
/// substring match against the raw expression AND against a
/// comment/whitespace-stripped form. (FR-WEB-07, FR-SAFETY-01)
///
/// # Threat model — guardrail, NOT a sandbox
///
/// This gate exists to stop *accidental or naive* destructive
/// evaluates under the operator-opted safe profile (an agent pasting
/// `document.cookie = ...`, a prompt-injected one-liner). It is
/// defense-in-depth on top of the real containment layers (Chromium's
/// sandbox, the vault grant mechanism, budgets) and makes NO claim of
/// containing a determined adversary: JavaScript is far too dynamic
/// for substring matching to be a security boundary. Known, accepted
/// bypasses include dynamic property access
/// (`window["loc" + "ation"]`), string/unicode-escape obfuscation, and
/// aliasing (`const d = document; d.cookie`). Do NOT extend this list
/// expecting sandbox semantics; if a hard guarantee is needed it must
/// be enforced browser-side (e.g. CDP-level controls), not here.
///
/// Matching is deliberately one-sided: token-separator tricks that do
/// NOT change JS semantics (whitespace around `.`/`(`, interleaved
/// comments — `document . cookie`, `eval/**/(x)`) are normalized away
/// before the second pass, while case is left alone because JS member
/// access is case-sensitive (`Document.Cookie` never reaches the
/// cookie jar — folding would only add false positives). Over-blocking
/// (e.g. a pattern inside a string literal) is acceptable for this
/// profile; under-blocking relative to the raw match never happens
/// (pass 1 is the historical raw substring check, unchanged).
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
    // Additional patterns (operator's regex patterns, translated to
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
    /// Expression matched a pattern in `EVALUATE_DENYLIST`.
    EvaluateDenylistMatch,
    /// download target path is outside the session-scoped dir.
    DownloadPathBlocked { candidate_path: String },
}

/// Stateless safety policy checker. All functions are pure — no I/O, no
/// allocations beyond the optional return value.
pub struct SafetyPolicy;

impl SafetyPolicy {
    /// Check whether `expression` is permitted under `profile`.
    ///
    /// Returns `Some(PolicyViolation::EvaluateDenylistMatch)` when
    /// `profile == Safe` and [`find_denylist_match`] finds a pattern from
    /// `EVALUATE_DENYLIST`. Returns `None` when the expression is allowed.
    ///
    /// Used by `EvaluateVerb::execute` as a pre-CDP gate.
    pub fn check_evaluate(profile: SafetyProfile, expression: &str) -> Option<PolicyViolation> {
        if profile != SafetyProfile::Safe {
            return None;
        }
        find_denylist_match(expression).map(|_| PolicyViolation::EvaluateDenylistMatch)
    }

    /// Cookie-verb safety stubs (v0.9.5 / D9). The authoritative gate
    /// lives in the daemon layer per the EvaluateVerb dead-code pattern;
    /// these verb-level checks exist for symmetry with `check_evaluate`
    /// and for future-proofing. Today they always return `None`
    /// (allow-all) under both Default and Safe — the structured
    /// `set_cookies` path is the operator-blessed legitimate channel for
    /// cookie mutation, and the `document.cookie=` write-side block in
    /// `EVALUATE_DENYLIST` stays as the XSS-shaped-escape-hatch deterrent.
    pub fn check_set_cookies(_profile: SafetyProfile) -> Option<PolicyViolation> {
        None
    }
    pub fn check_get_cookies(_profile: SafetyProfile) -> Option<PolicyViolation> {
        None
    }
    pub fn check_clear_cookies(_profile: SafetyProfile) -> Option<PolicyViolation> {
        None
    }
    pub fn check_delete_cookies(_profile: SafetyProfile) -> Option<PolicyViolation> {
        None
    }

    /// Check whether `path` is strictly inside `session_downloads_dir`.
    ///
    /// Both arguments are lexically normalised (`.` skipped, `..` resolved
    /// against the preceding component — no filesystem access, symlinks
    /// not chased) and compared component-wise, so:
    /// - `<dir>/../../etc/passwd` is rejected (escapes after
    ///   normalisation);
    /// - `/foo/barbaz` does NOT match a `/foo/bar` base (no
    ///   prefix-without-separator smuggling);
    /// - relative paths and paths that climb above `/` are rejected
    ///   (fail closed — containment can't be proven lexically).
    ///
    /// Used by the host shim to validate download targets under
    /// `SafetyProfile::Safe`.
    pub fn is_session_scoped_path(path: &str, session_downloads_dir: &str) -> bool {
        is_strictly_under(path, session_downloads_dir)
    }

    /// Check whether `path` is strictly under `loom_data_root`.
    ///
    /// Same lexical-normalisation + component-wise comparison semantics
    /// as [`SafetyPolicy::is_session_scoped_path`]. Used by host-side
    /// write guards to ensure all session data lives under `~/.loom/` or
    /// `$XDG_DATA_HOME/loom`.
    pub fn is_loom_data_path(path: &str, loom_data_root: &str) -> bool {
        is_strictly_under(path, loom_data_root)
    }
}

/// Find the first `EVALUATE_DENYLIST` pattern present in `expression`,
/// returning it for receipt/audit detail. This is the single matching
/// routine for the safe-profile evaluate gate (both the daemon-side
/// authoritative gate and `check_evaluate` call it).
///
/// Two passes, strictly monotonic (pass 2 can only ADD detections over
/// the historical raw check, never remove one):
/// 1. raw substring match against the expression as received;
/// 2. substring match against the expression with JS comments
///    (`/* … */`, `// … <eol>`) and all whitespace stripped, defeating
///    token-separator smuggling like `document . cookie`,
///    `eval/**/('…')` or `document.// hop\ncookie` — all of which are
///    semantically identical to the unobfuscated form in JS.
///
/// See the threat-model note on [`EVALUATE_DENYLIST`] for what this
/// deliberately does NOT defend against.
pub fn find_denylist_match(expression: &str) -> Option<&'static str> {
    if let Some(p) = EVALUATE_DENYLIST.iter().find(|p| expression.contains(*p)) {
        return Some(*p);
    }
    let normalized = strip_comments_and_whitespace(expression);
    EVALUATE_DENYLIST
        .iter()
        .find(|p| normalized.contains(*p))
        .copied()
}

/// Remove JS comments and all whitespace from `expression` for the
/// second denylist pass. Comments and whitespace are token separators
/// that never change the semantics of the member accesses / calls the
/// denylist targets, so stripping them exposes split-token smuggling.
///
/// Deliberately NOT string-literal-aware (that would be a lexer):
/// mangling text inside string literals can only cause over-matching,
/// which is acceptable for this gate — pass 1 already preserved every
/// raw match, so this pass is purely additive. An unterminated `/*`
/// comment is stripped to the end of input (it is a JS syntax error;
/// nothing after it would execute anyway).
fn strip_comments_and_whitespace(expression: &str) -> String {
    let mut out = String::with_capacity(expression.len());
    let mut chars = expression.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' {
            match chars.peek() {
                Some('*') => {
                    chars.next();
                    let mut prev = '\0';
                    for c2 in chars.by_ref() {
                        if prev == '*' && c2 == '/' {
                            break;
                        }
                        prev = c2;
                    }
                }
                Some('/') => {
                    chars.next();
                    for c2 in chars.by_ref() {
                        if c2 == '\n' {
                            break;
                        }
                    }
                }
                _ => out.push(c),
            }
        } else if !c.is_whitespace() {
            out.push(c);
        }
    }
    out
}

/// Lexically resolve an absolute `/`-separated path into its components:
/// empty components (`//`) and `.` are skipped, `..` pops the previous
/// component. Pure string processing — no filesystem access, so symlinks
/// are NOT resolved.
///
/// Returns `None` (fail closed) when the path is relative or when a `..`
/// component climbs above the root.
fn normalized_components(path: &str) -> Option<alloc::vec::Vec<&str>> {
    if !path.starts_with('/') {
        return None;
    }
    let mut comps: alloc::vec::Vec<&str> = alloc::vec::Vec::new();
    for comp in path.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                // Popping past the root means the path escapes any
                // conceivable base — reject outright.
                comps.pop()?;
            }
            c => comps.push(c),
        }
    }
    Some(comps)
}

/// `true` iff `path` normalises to a strict component-wise extension of
/// `base` (i.e. a file/dir *inside* `base`, never `base` itself). Either
/// side failing normalisation (relative input, `..` past root) yields
/// `false`.
fn is_strictly_under(path: &str, base: &str) -> bool {
    let (Some(p), Some(b)) = (normalized_components(path), normalized_components(base)) else {
        return false;
    };
    p.len() > b.len() && p[..b.len()] == b[..]
}
