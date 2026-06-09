//! Pure reaper decision functions — no I/O, no signals, no locks.
//!
//! These encode the intake- and plan-council safety invariants as data transforms so they
//! are exhaustively unit-testable (`loom-daemon/tests/reaper_decide.rs`). All real-world
//! effects (scanning `$TMPDIR`, reading pidfiles, sending signals, closing sessions) live in
//! `proc.rs` / `mod.rs`; this module only *decides* what the effects should target.
//!
//! Safety invariants reflected here:
//! - C5: a session with an in-flight action is NEVER an idle victim (no mid-action eviction).
//! - C2: an orphan dir whose session is still live, or which is younger than one sweep
//!   interval (grace-before-kill), is NEVER classified as an orphan.
//! - C4: a symlink / non-directory entry is NEVER an orphan (the scanner also pre-filters
//!   unsafe dir names; this is defense-in-depth).

use std::collections::HashSet;
use std::path::PathBuf;

/// In-memory snapshot of a session for idle-eviction decisions.
#[derive(Debug, Clone)]
pub struct SessionView {
    pub id: String,
    /// Unix epoch milliseconds of the session's last action activity.
    pub last_activity_ms: u64,
    /// Number of actions currently executing for this session.
    pub in_flight: u32,
    /// True iff the session is in the `Active` state.
    pub is_active: bool,
}

/// A `$TMPDIR/loom-chromium-<id>` directory entry, pre-parsed by the scanner.
#[derive(Debug, Clone)]
pub struct BrowserDirEntry {
    /// Session id parsed from the `loom-chromium-<id>` directory name.
    pub session_id: String,
    pub path: PathBuf,
    /// Directory mtime in Unix epoch milliseconds.
    pub mtime_ms: u64,
    pub is_symlink: bool,
    pub is_dir: bool,
}

/// A session paired with its browser-process liveness, for zombie detection.
#[derive(Debug, Clone)]
pub struct ZombieView {
    pub id: String,
    pub is_active: bool,
    /// True iff the session's Chromium pid is still alive.
    pub browser_pid_alive: bool,
}

/// Active sessions idle for longer than `ttl_ms` with NO in-flight action.
///
/// `ttl_ms == 0` disables eviction (returns empty). Sessions with `in_flight > 0` are spared
/// regardless of idle time (C5) — the actual close path re-checks this under the status lock.
pub fn idle_victims(active: &[SessionView], ttl_ms: u64, now_ms: u64) -> Vec<String> {
    if ttl_ms == 0 {
        return Vec::new();
    }
    active
        .iter()
        .filter(|s| s.is_active && s.in_flight == 0)
        .filter(|s| now_ms.saturating_sub(s.last_activity_ms) >= ttl_ms)
        .map(|s| s.id.clone())
        .collect()
}

/// Browser dirs that are safe to GC: real directory (not a symlink), session not live, and
/// aged past `min_age_ms` (grace-before-kill). The fresh live-set recheck immediately before
/// the kill (in `mod.rs`) is the PRIMARY guard; the age grace defends the create-race window.
pub fn classify_orphans(
    entries: &[BrowserDirEntry],
    live: &HashSet<String>,
    now_ms: u64,
    min_age_ms: u64,
) -> Vec<String> {
    entries
        .iter()
        .filter(|e| e.is_dir && !e.is_symlink)
        .filter(|e| !live.contains(&e.session_id))
        .filter(|e| now_ms.saturating_sub(e.mtime_ms) >= min_age_ms)
        .map(|e| e.session_id.clone())
        .collect()
}

/// Active sessions whose Chromium process is already dead — zombies to close.
pub fn zombie_victims(views: &[ZombieView]) -> Vec<String> {
    views
        .iter()
        .filter(|v| v.is_active && !v.browser_pid_alive)
        .map(|v| v.id.clone())
        .collect()
}
