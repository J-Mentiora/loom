//! RED test (TDD) for the reaper's PURE decision functions.
//!
//! These functions encode the intake-council safety invariants (C2/C5) as pure logic
//! over in-memory views — no filesystem, no signals — so they are exhaustively unit-testable.
//! Until `loom_daemon::reaper` exists this file fails to COMPILE, which is the intended RED
//! state for `specs/2026-06-09-session-chromium-reaper`.
//!
//! Contract (implemented in Phase 3, `loom-daemon/src/reaper/decide.rs`):
//!   idle_victims(&[SessionView], ttl_ms, now_ms) -> Vec<String>
//!   classify_orphans(&[BrowserDirEntry], &HashSet<String>, now_ms, min_age_ms) -> Vec<String>
//!   zombie_victims(&[ZombieView]) -> Vec<String>

use std::collections::HashSet;
use std::path::PathBuf;

use loom_daemon::reaper::decide::{
    classify_orphans, idle_victims, zombie_victims, BrowserDirEntry, SessionView, ZombieView,
};

fn sv(id: &str, last_activity_ms: u64, in_flight: u32, is_active: bool) -> SessionView {
    SessionView {
        id: id.to_string(),
        last_activity_ms,
        in_flight,
        is_active,
    }
}

// === idle_victims ============================================================

#[test]
fn t1_idle_active_no_inflight_is_victim() {
    let now = 100_000;
    let ttl = 30_000;
    let active = vec![sv("s-idle", 10_000, 0, true)]; // idle for 90s > 30s ttl
    assert_eq!(idle_victims(&active, ttl, now), vec!["s-idle".to_string()]);
}

#[test]
fn t2_idle_with_inflight_action_is_never_evicted() {
    // C5: a session running a long single action must not be reaped mid-flight.
    let now = 100_000;
    let active = vec![sv("s-busy", 10_000, 1, true)];
    assert!(idle_victims(&active, 30_000, now).is_empty());
}

#[test]
fn t3_recently_active_is_not_a_victim() {
    let now = 100_000;
    let active = vec![sv("s-fresh", 95_000, 0, true)]; // idle 5s < 30s
    assert!(idle_victims(&active, 30_000, now).is_empty());
}

#[test]
fn t4_ttl_zero_disables_eviction() {
    let now = 100_000;
    let active = vec![sv("s-old", 0, 0, true)];
    assert!(idle_victims(&active, 0, now).is_empty());
}

#[test]
fn t5_non_active_session_is_never_a_victim() {
    let now = 100_000;
    let active = vec![sv("s-closed", 0, 0, false)];
    assert!(idle_victims(&active, 30_000, now).is_empty());
}

// === classify_orphans ========================================================

fn bde(session_id: &str, mtime_ms: u64, is_symlink: bool, is_dir: bool) -> BrowserDirEntry {
    BrowserDirEntry {
        session_id: session_id.to_string(),
        path: PathBuf::from(format!("/tmp/loom-chromium-{session_id}")),
        mtime_ms,
        is_symlink,
        is_dir,
    }
}

#[test]
fn t6_dead_session_aged_dir_is_orphan() {
    let now = 100_000;
    let live: HashSet<String> = HashSet::new();
    let entries = vec![bde("dead", 10_000, false, true)]; // aged 90s
    assert_eq!(
        classify_orphans(&entries, &live, now, 60_000),
        vec!["dead".to_string()]
    );
}

#[test]
fn t7_live_session_dir_is_never_orphan() {
    // C2: never kill a live session's browser.
    let now = 100_000;
    let live: HashSet<String> = ["live".to_string()].into_iter().collect();
    let entries = vec![bde("live", 10_000, false, true)];
    assert!(classify_orphans(&entries, &live, now, 60_000).is_empty());
}

#[test]
fn t8_young_dir_is_not_orphan_grace_before_kill() {
    // C2: a just-created session's dir (younger than one sweep interval) is spared.
    let now = 100_000;
    let live: HashSet<String> = HashSet::new();
    let entries = vec![bde("newish", 90_000, false, true)]; // aged 10s < 60s grace
    assert!(classify_orphans(&entries, &live, now, 60_000).is_empty());
}

#[test]
fn t9_symlink_entry_is_never_orphan() {
    // C4: never follow a symlink into remove_dir_all.
    let now = 100_000;
    let live: HashSet<String> = HashSet::new();
    let entries = vec![bde("evil", 10_000, true, false)];
    assert!(classify_orphans(&entries, &live, now, 60_000).is_empty());
}

// === zombie_victims ==========================================================

#[test]
fn t11_active_session_with_dead_browser_is_zombie() {
    let views = vec![ZombieView {
        id: "z".to_string(),
        is_active: true,
        browser_pid_alive: false,
    }];
    assert_eq!(zombie_victims(&views), vec!["z".to_string()]);
}

#[test]
fn t12_active_session_with_live_browser_is_not_zombie() {
    let views = vec![ZombieView {
        id: "ok".to_string(),
        is_active: true,
        browser_pid_alive: true,
    }];
    assert!(zombie_victims(&views).is_empty());
}
