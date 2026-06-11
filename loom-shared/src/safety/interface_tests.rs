// Interface tests for SafetyPolicy.

use super::safety::{PolicyViolation, SafetyPolicy, SafetyProfile};

// --- Safe profile blocks destructive evaluate ---

#[test]
fn safe_profile_blocks_cookie_write() {
    // action.evaluate({js: "document.cookie = ''"}) is blocked
    let result = SafetyPolicy::check_evaluate(SafetyProfile::Safe, "document.cookie = ''");
    assert_eq!(result, Some(PolicyViolation::EvaluateDenylistMatch));
}

#[test]
fn safe_profile_blocks_local_storage() {
    let result =
        SafetyPolicy::check_evaluate(SafetyProfile::Safe, "localStorage.setItem('k', 'v')");
    assert_eq!(result, Some(PolicyViolation::EvaluateDenylistMatch));
}

#[test]
fn safe_profile_blocks_session_storage() {
    let result = SafetyPolicy::check_evaluate(SafetyProfile::Safe, "sessionStorage.clear()");
    assert_eq!(result, Some(PolicyViolation::EvaluateDenylistMatch));
}

#[test]
fn safe_profile_blocks_eval() {
    let result = SafetyPolicy::check_evaluate(SafetyProfile::Safe, "eval('alert(1)')");
    assert_eq!(result, Some(PolicyViolation::EvaluateDenylistMatch));
}

#[test]
fn default_profile_allows_destructive_expressions() {
    // only safe profile blocks; default profile is unrestricted
    let result = SafetyPolicy::check_evaluate(SafetyProfile::Default, "document.cookie = ''");
    assert_eq!(result, None);
}

#[test]
fn safe_profile_allows_benign_expression() {
    let result = SafetyPolicy::check_evaluate(SafetyProfile::Safe, "document.title");
    assert_eq!(result, None);
}

#[test]
fn safe_profile_allows_read_only_dom_query() {
    let result = SafetyPolicy::check_evaluate(
        SafetyProfile::Safe,
        "document.querySelector('h1').textContent",
    );
    assert_eq!(result, None);
}

// --- Additions: window.location + serviceWorker ---

#[test]
fn safe_profile_blocks_window_location_assignment() {
    // Reproducer: the operator's exact bug.
    let result = SafetyPolicy::check_evaluate(
        SafetyProfile::Safe,
        "window.location.href = \"https://evil.example.com\"",
    );
    assert_eq!(result, Some(PolicyViolation::EvaluateDenylistMatch));
}

#[test]
fn safe_profile_blocks_window_location_replace() {
    let result = SafetyPolicy::check_evaluate(
        SafetyProfile::Safe,
        "window.location.replace('https://evil.example.com')",
    );
    assert_eq!(result, Some(PolicyViolation::EvaluateDenylistMatch));
}

#[test]
fn safe_profile_blocks_console_log_of_window_location_intentional_defense_in_depth() {
    // INTENTIONAL: substring matching against `window.location` is broader
    // than the operator's regex `window\.location[ \t]*=`. It catches reads
    // (e.g. `console.log(window.location)`) too. For safe profile this is
    // acceptable defense-in-depth — operator opted into restrictions, and
    // exfiltrating `window.location.href` is itself a leak vector.
    //
    // This test pins the broad-match behavior so it can't be silently relaxed.
    let result = SafetyPolicy::check_evaluate(SafetyProfile::Safe, "console.log(window.location)");
    assert_eq!(result, Some(PolicyViolation::EvaluateDenylistMatch));
}

#[test]
fn safe_profile_blocks_service_worker_register() {
    let result = SafetyPolicy::check_evaluate(
        SafetyProfile::Safe,
        "navigator.serviceWorker.register('/sw.js')",
    );
    assert_eq!(result, Some(PolicyViolation::EvaluateDenylistMatch));
}

#[test]
fn safe_profile_allows_service_worker_feature_detect() {
    // Deliberate carve-out: tightening the denylist pattern from
    // `navigator.serviceWorker` to `navigator.serviceWorker.register`
    // was specifically to leave this common defensive pattern working
    // under safe profile. If this test breaks, the pattern was widened
    // too far.
    let result = SafetyPolicy::check_evaluate(
        SafetyProfile::Safe,
        "if ('serviceWorker' in navigator) { /* nothing */ }",
    );
    assert_eq!(result, None);
}

#[test]
fn safe_profile_blocks_destructive_evaluate_via_window_location_full_repro() {
    // End-to-end on the `check_evaluate` boundary —
    // mirrors the exact test the daemon-layer gate runs. If the
    // production gate ever diverges from this expectation, the substring
    // set is the source of truth.
    let result = SafetyPolicy::check_evaluate(
        SafetyProfile::Safe,
        "window.location = 'https://evil.example.com'",
    );
    assert_eq!(result, Some(PolicyViolation::EvaluateDenylistMatch));
}

#[test]
fn default_profile_allows_window_location_assignment_regression_guard() {
    // Regression guard — without --profile safe, the session must NOT
    // block destructive evaluates.
    let result = SafetyPolicy::check_evaluate(
        SafetyProfile::Default,
        "window.location.href = \"https://evil.example.com\"",
    );
    assert_eq!(result, None);
}

// --- Denylist bypass variants (audit 2026-06-10) ---
// The gate was raw-substring only; whitespace and comments are JS token
// separators that don't change the semantics of a member access, so the
// normalized second pass of `find_denylist_match` must catch them.

#[test]
fn safe_profile_blocks_whitespace_split_member_access() {
    // `document . cookie` is semantically identical to `document.cookie`.
    let result = SafetyPolicy::check_evaluate(SafetyProfile::Safe, "document . cookie = 'x'");
    assert_eq!(result, Some(PolicyViolation::EvaluateDenylistMatch));
}

#[test]
fn safe_profile_blocks_newline_split_member_access() {
    let result = SafetyPolicy::check_evaluate(SafetyProfile::Safe, "document\n  .cookie = 'x'");
    assert_eq!(result, Some(PolicyViolation::EvaluateDenylistMatch));
}

#[test]
fn safe_profile_blocks_block_comment_split_member_access() {
    // `document/**/.cookie` — a block comment is a token separator.
    let result = SafetyPolicy::check_evaluate(SafetyProfile::Safe, "document/**/.cookie = 'x'");
    assert_eq!(result, Some(PolicyViolation::EvaluateDenylistMatch));
}

#[test]
fn safe_profile_blocks_line_comment_split_member_access() {
    let result =
        SafetyPolicy::check_evaluate(SafetyProfile::Safe, "document. // hop\ncookie = 'x'");
    assert_eq!(result, Some(PolicyViolation::EvaluateDenylistMatch));
}

#[test]
fn safe_profile_blocks_whitespace_before_call_paren() {
    assert_eq!(
        SafetyPolicy::check_evaluate(SafetyProfile::Safe, "eval ('alert(1)')"),
        Some(PolicyViolation::EvaluateDenylistMatch)
    );
    assert_eq!(
        SafetyPolicy::check_evaluate(SafetyProfile::Safe, "document.write ('<b>x</b>')"),
        Some(PolicyViolation::EvaluateDenylistMatch)
    );
}

#[test]
fn safe_profile_blocks_comment_split_window_location() {
    let result = SafetyPolicy::check_evaluate(
        SafetyProfile::Safe,
        "window/* sneak */. location = 'https://evil.example.com'",
    );
    assert_eq!(result, Some(PolicyViolation::EvaluateDenylistMatch));
}

#[test]
fn find_denylist_match_reports_the_matched_pattern() {
    // The daemon's authoritative gate puts the matched pattern in the
    // receipt detail — pin the returned value for both passes.
    use super::safety::find_denylist_match;
    assert_eq!(
        find_denylist_match("window.location = 'x'"),
        Some("window.location")
    );
    assert_eq!(
        find_denylist_match("window . location = 'x'"),
        Some("window.location")
    );
    assert_eq!(find_denylist_match("document.title"), None);
}

#[test]
fn safe_profile_allows_benign_division_and_comments() {
    // The comment/whitespace stripper must not flag ordinary code.
    assert_eq!(
        SafetyPolicy::check_evaluate(SafetyProfile::Safe, "const r = a / b / c;"),
        None
    );
    assert_eq!(
        SafetyPolicy::check_evaluate(
            SafetyProfile::Safe,
            "document.title /* read-only probe */ // end",
        ),
        None
    );
}

#[test]
fn safe_profile_feature_detect_carveout_survives_normalization() {
    // The serviceWorker feature-detect carve-out must hold for the
    // normalized pass too.
    let result = SafetyPolicy::check_evaluate(
        SafetyProfile::Safe,
        "if ('serviceWorker' in navigator) { /* nothing */ }",
    );
    assert_eq!(result, None);
}

#[test]
fn denylist_does_not_catch_dynamic_property_access_by_design() {
    // Honest threat-model pin: dynamic property access is a KNOWN,
    // ACCEPTED bypass (this gate is a guardrail, not a sandbox — see
    // the EVALUATE_DENYLIST doc). If this test ever starts failing,
    // the matching strategy changed fundamentally; re-read the threat
    // model before celebrating.
    let result = SafetyPolicy::check_evaluate(
        SafetyProfile::Safe,
        "window['loc' + 'ation'] = 'https://evil.example.com'",
    );
    assert_eq!(result, None);
}

// --- Safe profile restricts download paths ---

#[test]
fn download_outside_session_dir_is_not_scoped() {
    // Download to /tmp/foo.bin is rejected
    assert!(!SafetyPolicy::is_session_scoped_path(
        "/tmp/foo.bin",
        "/Users/alice/.loom/sessions/01abc/downloads",
    ));
}

#[test]
fn download_inside_session_dir_is_scoped() {
    assert!(SafetyPolicy::is_session_scoped_path(
        "/Users/alice/.loom/sessions/01abc/downloads/file.pdf",
        "/Users/alice/.loom/sessions/01abc/downloads",
    ));
}

#[test]
fn download_to_home_dir_is_not_scoped() {
    assert!(!SafetyPolicy::is_session_scoped_path(
        "/Users/alice/Downloads/file.pdf",
        "/Users/alice/.loom/sessions/01abc/downloads",
    ));
}

#[test]
fn download_path_check_handles_trailing_slash() {
    // Downloads dir with trailing slash should still work correctly
    assert!(SafetyPolicy::is_session_scoped_path(
        "/Users/alice/.loom/sessions/01abc/downloads/sub/file.bin",
        "/Users/alice/.loom/sessions/01abc/downloads/",
    ));
}

// --- Traversal / normalization regressions (audit 2026-06-10) ---
// Both guards are lexical security checks; a `..` that escapes the base
// after normalization, or a sibling dir sharing the base as a raw string
// prefix, must be rejected.

#[test]
fn download_dotdot_escape_is_not_scoped() {
    // Starts with the base prefix as a raw string, but normalizes to
    // /etc/passwd — the audit's exact escape shape.
    assert!(!SafetyPolicy::is_session_scoped_path(
        "/Users/alice/.loom/sessions/01abc/downloads/../../../../../../etc/passwd",
        "/Users/alice/.loom/sessions/01abc/downloads",
    ));
}

#[test]
fn download_single_dotdot_to_sibling_is_not_scoped() {
    assert!(!SafetyPolicy::is_session_scoped_path(
        "/Users/alice/.loom/sessions/01abc/downloads/../manifest.wal",
        "/Users/alice/.loom/sessions/01abc/downloads",
    ));
}

#[test]
fn download_dotdot_resolving_back_inside_is_scoped() {
    // `sub/../file.pdf` normalizes to `<base>/file.pdf` — still inside.
    assert!(SafetyPolicy::is_session_scoped_path(
        "/Users/alice/.loom/sessions/01abc/downloads/sub/../file.pdf",
        "/Users/alice/.loom/sessions/01abc/downloads",
    ));
}

#[test]
fn download_dotdot_back_to_base_itself_is_not_scoped() {
    // `<base>/sub/..` normalizes to the base dir itself, not a file
    // inside it — strict containment required.
    assert!(!SafetyPolicy::is_session_scoped_path(
        "/Users/alice/.loom/sessions/01abc/downloads/sub/..",
        "/Users/alice/.loom/sessions/01abc/downloads",
    ));
}

#[test]
fn download_prefix_without_separator_is_not_scoped() {
    // The prefix-without-separator trap: /foo/barbaz must not match a
    // /foo/bar base.
    assert!(!SafetyPolicy::is_session_scoped_path(
        "/Users/alice/.loom/sessions/01abc/downloadsevil/file.pdf",
        "/Users/alice/.loom/sessions/01abc/downloads",
    ));
}

#[test]
fn download_curdir_and_double_slash_components_are_scoped() {
    assert!(SafetyPolicy::is_session_scoped_path(
        "/Users/alice/.loom/sessions/01abc/downloads/./file.pdf",
        "/Users/alice/.loom/sessions/01abc/downloads",
    ));
    assert!(SafetyPolicy::is_session_scoped_path(
        "/Users/alice/.loom/sessions/01abc/downloads//file.pdf",
        "/Users/alice/.loom/sessions/01abc/downloads",
    ));
}

#[test]
fn download_relative_path_is_not_scoped() {
    // Containment can't be proven lexically for a relative path — fail
    // closed.
    assert!(!SafetyPolicy::is_session_scoped_path(
        "downloads/file.pdf",
        "/Users/alice/.loom/sessions/01abc/downloads",
    ));
}

#[test]
fn download_path_climbing_above_root_is_not_scoped() {
    assert!(!SafetyPolicy::is_session_scoped_path(
        "/../Users/alice/.loom/sessions/01abc/downloads/file.pdf",
        "/Users/alice/.loom/sessions/01abc/downloads",
    ));
}

#[test]
fn loom_data_path_rejects_dotdot_escape() {
    // The audit's exact escape shape for the data-root guard.
    assert!(!SafetyPolicy::is_loom_data_path(
        "/Users/alice/.loom/../../etc/shadow",
        "/Users/alice/.loom",
    ));
}

#[test]
fn loom_data_path_rejects_prefix_smuggled_sibling() {
    assert!(!SafetyPolicy::is_loom_data_path(
        "/Users/alice/.loom-evil/store/blob",
        "/Users/alice/.loom",
    ));
}

#[test]
fn loom_data_path_accepts_internal_dotdot_that_stays_inside() {
    assert!(SafetyPolicy::is_loom_data_path(
        "/Users/alice/.loom/sessions/../store/ab/cdef.blob",
        "/Users/alice/.loom",
    ));
}

#[test]
fn loom_data_path_rejects_base_that_escapes_after_normalization() {
    // A base that itself climbs above root can't anchor a containment
    // claim — fail closed.
    assert!(!SafetyPolicy::is_loom_data_path("/etc/passwd", "/../etc"));
}

// --- All session data stays under ~/.loom/ ---

#[test]
fn loom_data_path_accepts_under_root() {
    // sessions/<id>/manifest.wal is under ~/.loom/
    assert!(SafetyPolicy::is_loom_data_path(
        "/Users/alice/.loom/sessions/01abc/manifest.wal",
        "/Users/alice/.loom",
    ));
}

#[test]
fn loom_data_path_accepts_content_store() {
    assert!(SafetyPolicy::is_loom_data_path(
        "/Users/alice/.loom/store/ab/cdef.blob",
        "/Users/alice/.loom",
    ));
}

#[test]
fn loom_data_path_rejects_tmp() {
    // /tmp writes are not allowed
    assert!(!SafetyPolicy::is_loom_data_path(
        "/tmp/foo.txt",
        "/Users/alice/.loom",
    ));
}

#[test]
fn loom_data_path_rejects_documents() {
    // ~/Documents writes are not allowed
    assert!(!SafetyPolicy::is_loom_data_path(
        "/Users/alice/Documents/file.txt",
        "/Users/alice/.loom",
    ));
}
