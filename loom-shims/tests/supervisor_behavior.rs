// Behavior tests for Supervisor locale env — TDD.
//
// AC coverage:
//   AC-DET-06.1: test_lc_all_set_in_chromium_env
//                test_lang_set_in_chromium_env
//                test_locale_scrub_removes_lc_messages
//                test_restart_allowed_within_budget

use loom_shims::supervisor::supervisor::{locale_scrub_env, restart_allowed, RestartBudget};
use std::time::{Duration, Instant};

// === AC-DET-06.1: locale-scrubbed env ===

#[test]
fn test_lc_all_set_in_chromium_env() {
    let (set_pairs, _) = locale_scrub_env();
    let lc_all = set_pairs.iter().find(|(k, _)| *k == "LC_ALL");
    assert!(lc_all.is_some(), "locale_scrub_env must include LC_ALL");
    assert_eq!(
        lc_all.unwrap().1,
        "C.UTF-8",
        "LC_ALL must be set to C.UTF-8 (AC-DET-06.1)"
    );
}

#[test]
fn test_lang_set_in_chromium_env() {
    let (set_pairs, _) = locale_scrub_env();
    let lang = set_pairs.iter().find(|(k, _)| *k == "LANG");
    assert!(lang.is_some(), "locale_scrub_env must include LANG");
    assert_eq!(lang.unwrap().1, "C.UTF-8");
}

#[test]
fn test_locale_scrub_removes_lc_messages() {
    let (_, remove_keys) = locale_scrub_env();
    assert!(
        remove_keys.contains(&"LC_MESSAGES"),
        "must remove LC_MESSAGES"
    );
    assert!(
        remove_keys.contains(&"LC_NUMERIC"),
        "must remove LC_NUMERIC"
    );
    assert!(remove_keys.contains(&"LC_TIME"), "must remove LC_TIME");
}

// === Restart budget helper ===

#[test]
fn test_restart_allowed_within_budget() {
    let budget = RestartBudget {
        max_within_window: 3,
        window: Duration::from_secs(60),
    };
    let now = Instant::now();
    // Two restarts within window — third should be allowed
    let history = vec![now - Duration::from_secs(10), now - Duration::from_secs(5)];
    assert!(restart_allowed(&history, now, budget));
}

#[test]
fn test_restart_denied_when_budget_exhausted() {
    let budget = RestartBudget {
        max_within_window: 3,
        window: Duration::from_secs(60),
    };
    let now = Instant::now();
    let history = vec![
        now - Duration::from_secs(20),
        now - Duration::from_secs(10),
        now - Duration::from_secs(5),
    ];
    assert!(!restart_allowed(&history, now, budget));
}

#[test]
fn test_restart_allowed_after_old_entries_expire() {
    let budget = RestartBudget {
        max_within_window: 3,
        window: Duration::from_secs(60),
    };
    let now = Instant::now();
    // Three restarts, all older than the 60s window
    let history = vec![
        now - Duration::from_secs(120),
        now - Duration::from_secs(90),
        now - Duration::from_secs(70),
    ];
    assert!(restart_allowed(&history, now, budget));
}
