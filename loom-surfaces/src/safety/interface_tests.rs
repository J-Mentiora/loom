// Interface tests for SafetyPolicy.
// AC-WEB-07.1 / AC-WEB-07.2 / AC-SAFETY-02.1

use super::safety::{PolicyViolation, SafetyPolicy, SafetyProfile};

// --- AC-WEB-07.1: safe profile blocks destructive evaluate ---

#[test]
fn safe_profile_blocks_cookie_write() {
    // AC-WEB-07.1: action.evaluate({js: "document.cookie = ''"}) is blocked
    let result = SafetyPolicy::check_evaluate(
        SafetyProfile::Safe,
        "document.cookie = ''",
    );
    assert_eq!(result, Some(PolicyViolation::EvaluateDenylistMatch));
}

#[test]
fn safe_profile_blocks_local_storage() {
    let result = SafetyPolicy::check_evaluate(
        SafetyProfile::Safe,
        "localStorage.setItem('k', 'v')",
    );
    assert_eq!(result, Some(PolicyViolation::EvaluateDenylistMatch));
}

#[test]
fn safe_profile_blocks_session_storage() {
    let result = SafetyPolicy::check_evaluate(
        SafetyProfile::Safe,
        "sessionStorage.clear()",
    );
    assert_eq!(result, Some(PolicyViolation::EvaluateDenylistMatch));
}

#[test]
fn safe_profile_blocks_eval() {
    let result = SafetyPolicy::check_evaluate(
        SafetyProfile::Safe,
        "eval('alert(1)')",
    );
    assert_eq!(result, Some(PolicyViolation::EvaluateDenylistMatch));
}

#[test]
fn default_profile_allows_destructive_expressions() {
    // AC-WEB-07.1: only safe profile blocks; default profile is unrestricted
    let result = SafetyPolicy::check_evaluate(
        SafetyProfile::Default,
        "document.cookie = ''",
    );
    assert_eq!(result, None);
}

#[test]
fn safe_profile_allows_benign_expression() {
    let result = SafetyPolicy::check_evaluate(
        SafetyProfile::Safe,
        "document.title",
    );
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

// --- AC-SAFEPROF-01 additions: window.location + serviceWorker ---

#[test]
fn safe_profile_blocks_window_location_assignment() {
    // AC-SAFEPROF-01 reproducer: the operator's exact bug.
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
    // This test pins the broad-match behavior so it can't be silently
    // relaxed (decisions.md Q10).
    let result = SafetyPolicy::check_evaluate(
        SafetyProfile::Safe,
        "console.log(window.location)",
    );
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
    // AC-SAFEPROF-01 deliberate carve-out (decisions.md Q11): tightening
    // the pattern from `navigator.serviceWorker` to
    // `navigator.serviceWorker.register` was specifically to leave this
    // common defensive pattern working under safe profile. If this test
    // breaks, the pattern was widened too far.
    let result = SafetyPolicy::check_evaluate(
        SafetyProfile::Safe,
        "if ('serviceWorker' in navigator) { /* nothing */ }",
    );
    assert_eq!(result, None);
}

#[test]
fn safe_profile_blocks_destructive_evaluate_via_window_location_full_repro() {
    // AC-SAFEPROF-01 end-to-end on the `check_evaluate` boundary —
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
    // AC-SAFEPROF-03: regression guard — without --profile safe, the
    // session must NOT block destructive evaluates.
    let result = SafetyPolicy::check_evaluate(
        SafetyProfile::Default,
        "window.location.href = \"https://evil.example.com\"",
    );
    assert_eq!(result, None);
}

// --- AC-WEB-07.2: safe profile restricts download paths ---

#[test]
fn download_outside_session_dir_is_not_scoped() {
    // AC-WEB-07.2: download to /tmp/foo.bin is rejected
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

// --- AC-SAFETY-02.1: all session data stays under ~/.loom/ ---

#[test]
fn loom_data_path_accepts_under_root() {
    // AC-SAFETY-02.1: sessions/<id>/manifest.wal is under ~/.loom/
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
    // AC-SAFETY-02.1: /tmp writes are not allowed
    assert!(!SafetyPolicy::is_loom_data_path(
        "/tmp/foo.txt",
        "/Users/alice/.loom",
    ));
}

#[test]
fn loom_data_path_rejects_documents() {
    // AC-SAFETY-02.1: ~/Documents writes are not allowed
    assert!(!SafetyPolicy::is_loom_data_path(
        "/Users/alice/Documents/file.txt",
        "/Users/alice/.loom",
    ));
}
