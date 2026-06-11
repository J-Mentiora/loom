//! Safety-policy contract tests — relocated from `loom-surfaces/tests/integration.rs`
//! (tests 1, 2, 3, 7) when `safety` moved into `loom-shared`. Exercises the
//! `SafetyPolicy` / `EVALUATE_DENYLIST` contract at the crate boundary.

use loom_shared::safety::{PolicyViolation, SafetyPolicy, SafetyProfile, EVALUATE_DENYLIST};

/// Contract: `check_evaluate` returns None when the expression does not match
/// any EVALUATE_DENYLIST pattern.
#[test]
fn test_safe_profile_allows_benign_expression() {
    let result = SafetyPolicy::check_evaluate(
        SafetyProfile::Safe,
        "document.getElementById('submit').click()",
    );
    assert_eq!(
        result, None,
        "benign expression must pass SafetyProfile::Safe; got {:?}",
        result
    );
}

/// Contract: `check_evaluate` returns EvaluateDenylistMatch when the expression
/// contains a EVALUATE_DENYLIST pattern under SafetyProfile::Safe. Covers all patterns.
#[test]
fn test_safe_profile_blocks_denylist_patterns() {
    for pattern in EVALUATE_DENYLIST {
        let expression = format!("const x = {}; x.getItem('key')", pattern);
        let result = SafetyPolicy::check_evaluate(SafetyProfile::Safe, &expression);
        assert_eq!(
            result,
            Some(PolicyViolation::EvaluateDenylistMatch),
            "denylist pattern {:?} must be blocked under SafetyProfile::Safe",
            pattern
        );
    }
}

/// Contract: `check_evaluate` returns None for any expression under
/// SafetyProfile::Default — the default profile has no evaluate restrictions.
#[test]
fn test_default_profile_allows_denylist_expressions() {
    for pattern in EVALUATE_DENYLIST {
        let expression = format!("{}.foo", pattern);
        let result = SafetyPolicy::check_evaluate(SafetyProfile::Default, &expression);
        assert_eq!(
            result, None,
            "SafetyProfile::Default must allow any expression; {:?} was blocked",
            pattern
        );
    }
}

/// Contract: `is_session_scoped_path` returns true only for paths under the
/// session downloads dir.
#[test]
fn test_is_session_scoped_path_positive_and_negative() {
    let base = "/tmp/loom/sessions/s1/downloads";

    assert!(
        SafetyPolicy::is_session_scoped_path("/tmp/loom/sessions/s1/downloads/file.pdf", base),
        "path under session_downloads_dir must be scoped"
    );
    assert!(
        !SafetyPolicy::is_session_scoped_path("/tmp/loom/sessions/s1/manifests/data.json", base),
        "sibling dir path must NOT be scoped"
    );
    assert!(
        !SafetyPolicy::is_session_scoped_path("/tmp/loom/sessions/s1", base),
        "parent dir must NOT be scoped"
    );
    assert!(
        SafetyPolicy::is_session_scoped_path(
            "/tmp/loom/sessions/s1/downloads/img.png",
            "/tmp/loom/sessions/s1/downloads/",
        ),
        "trailing-slash base must still match child paths"
    );
}

/// Contract: the path guards normalize `.` / `..` lexically before the
/// containment comparison and compare on component boundaries — a `..`
/// escape or a prefix-smuggled sibling dir is rejected (audit
/// 2026-06-10).
#[test]
fn test_path_guards_reject_traversal_and_prefix_smuggling() {
    let base = "/tmp/loom/sessions/s1/downloads";

    assert!(
        !SafetyPolicy::is_session_scoped_path(
            "/tmp/loom/sessions/s1/downloads/../../../../etc/passwd",
            base
        ),
        "`..` escape after the base prefix must NOT be scoped"
    );
    assert!(
        !SafetyPolicy::is_session_scoped_path("/tmp/loom/sessions/s1/downloads2/file", base),
        "prefix-without-separator sibling must NOT be scoped"
    );
    assert!(
        SafetyPolicy::is_session_scoped_path("/tmp/loom/sessions/s1/downloads/a/../file.bin", base),
        "`..` that resolves back inside the base must stay scoped"
    );
    assert!(
        !SafetyPolicy::is_loom_data_path("/tmp/loom/../../etc/shadow", "/tmp/loom"),
        "data-root `..` escape must NOT be in scope"
    );
}
