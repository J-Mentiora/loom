//! Process + filesystem primitives for the reaper (the I/O side of `decide.rs`).
//!
//! Every kill path is **fail-safe**: any uncertainty (missing pidfile, unreadable command
//! line, a pid whose cmdline does not match the expected user-data-dir) results in NO signal
//! being sent. We would rather leak a process for one more sweep than risk killing a live
//! session or an unrelated process (intake C3 / plan-council hardening B).

use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use super::decide::BrowserDirEntry;

/// Sidecar pidfile the shim supervisor writes into the user-data-dir. Holds the Chromium
/// process-group leader pid (== pgid, because the supervisor launches with `process_group(0)`).
/// Single source of truth lives in `loom-shared` so the writer (supervisor) and reader (this
/// reaper) can never drift.
pub use loom_shared::chromium_resolver::CHROMIUM_PIDFILE_NAME as PIDFILE_NAME;

/// Mirror of `loom_host`'s private `is_safe_session_id` — the guard before touching any
/// `loom-chromium-<id>` path. Duplicated (not shared) to avoid a new cross-crate dep for a
/// six-line predicate.
pub fn is_safe_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.len() <= 64
        && session_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// True iff `pid` names a live process (`kill(pid, 0)` succeeds or fails with `EPERM`).
pub fn pid_is_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: kill with signal 0 performs error checking without delivering a signal.
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return true;
    }
    // EPERM = process exists but we may not signal it (still "alive" for our purposes).
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// True iff `pid`'s command line contains `needle` (the user-data-dir path). Fail-safe:
/// returns `false` on any read error, so a caller never kills a pid it cannot verify.
pub fn pid_cmdline_contains(pid: i32, needle: &str) -> bool {
    if pid <= 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        // /proc/<pid>/cmdline is NUL-separated argv.
        std::fs::read(format!("/proc/{pid}/cmdline"))
            .map(|bytes| {
                String::from_utf8_lossy(&bytes)
                    .replace('\0', " ")
                    .contains(needle)
            })
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "linux"))]
    {
        // macOS / BSD: ask ps for the full command of exactly this pid.
        match std::process::Command::new("ps")
            .args(["-o", "command=", "-p", &pid.to_string()])
            .output()
        {
            Ok(out) if out.status.success() => {
                String::from_utf8_lossy(&out.stdout).contains(needle)
            }
            _ => false,
        }
    }
}

/// Outcome of a `terminate_group` attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillOutcome {
    /// The group was alive and is now dead (or became dead during the grace window).
    Terminated,
    /// Nothing was alive at `pgid` to begin with.
    AlreadyDead,
}

/// SIGTERM the process group, wait up to `grace` for it to exit, then SIGKILL any survivors.
///
/// `pgid` must be a process-group leader pid (the Chromium pid the supervisor pinned with
/// `process_group(0)`). Blocking — callers in async context wrap this in `spawn_blocking`.
pub fn terminate_group(pgid: i32, grace: Duration) -> KillOutcome {
    if pgid <= 0 || !pid_is_alive(pgid) {
        return KillOutcome::AlreadyDead;
    }
    // SAFETY: killpg delivers a signal to every process in the group; pgid is validated > 0.
    unsafe { libc::killpg(pgid, libc::SIGTERM) };

    let deadline = std::time::Instant::now() + grace;
    let poll = Duration::from_millis(50);
    while std::time::Instant::now() < deadline {
        if !pid_is_alive(pgid) {
            return KillOutcome::Terminated;
        }
        std::thread::sleep(poll);
    }
    if pid_is_alive(pgid) {
        // SAFETY: same as above; SIGKILL is uncatchable so survivors die.
        unsafe { libc::killpg(pgid, libc::SIGKILL) };
    }
    KillOutcome::Terminated
}

/// Read the Chromium pgid from `<udd>/loom-chromium.pid`. `None` when the file is absent or
/// unparseable — the caller then SKIPS the kill (fail-safe, no pattern-matching fallback).
pub fn read_pidfile(udd: &Path) -> Option<i32> {
    let raw = std::fs::read_to_string(udd.join(PIDFILE_NAME)).ok()?;
    raw.trim().parse::<i32>().ok().filter(|&p| p > 0)
}

/// Enumerate `<tmp_root>/loom-chromium-<safe-id>` directories into `BrowserDirEntry`s.
/// Unsafe dir names are skipped here (never reach `classify_orphans`). Best-effort: an
/// unreadable `tmp_root` yields an empty list rather than an error.
pub fn scan_browser_dirs(tmp_root: &Path) -> Vec<BrowserDirEntry> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(tmp_root) else {
        return out;
    };
    for entry in rd.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(session_id) = name.strip_prefix("loom-chromium-") else {
            continue;
        };
        if !is_safe_session_id(session_id) {
            continue;
        }
        // symlink_metadata: do NOT follow symlinks (C4 — never remove_dir_all through a link).
        let Ok(meta) = entry.path().symlink_metadata() else {
            continue;
        };
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        out.push(BrowserDirEntry {
            session_id: session_id.to_string(),
            path: entry.path(),
            mtime_ms,
            is_symlink: meta.file_type().is_symlink(),
            is_dir: meta.is_dir(),
        });
    }
    out
}

/// The temp root holding `loom-chromium-*` dirs (`$TMPDIR`), matching the host's
/// `std::env::temp_dir()` convention.
pub fn browser_tmp_root() -> PathBuf {
    std::env::temp_dir()
}

/// Remove an orphan user-data-dir behind the safe-name guard. Idempotent; never follows a
/// symlink (the entry was classified from `symlink_metadata`). Errors are logged, not fatal.
pub fn remove_orphan_dir(session_id: &str, path: &Path) {
    if !is_safe_session_id(session_id) {
        tracing::warn!(session = %session_id, "reaper: refusing to remove unsafe profile dir");
        return;
    }
    match std::fs::remove_dir_all(path) {
        Ok(()) => tracing::debug!(dir = %path.display(), "reaper: removed orphan profile dir"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            tracing::warn!(dir = %path.display(), error = %e, "reaper: orphan dir cleanup failed")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    #[test]
    fn safe_session_id_accepts_ulids_rejects_traversal() {
        assert!(is_safe_session_id("01HZX9QABCDEF0123456789ABC"));
        assert!(is_safe_session_id("a-b_c"));
        assert!(!is_safe_session_id(""));
        assert!(!is_safe_session_id("../etc"));
        assert!(!is_safe_session_id("has/slash"));
        assert!(!is_safe_session_id(&"x".repeat(65)));
    }

    #[test]
    fn pid_alive_true_for_self_false_for_bogus() {
        let me = std::process::id() as i32;
        assert!(pid_is_alive(me));
        assert!(!pid_is_alive(0));
        assert!(!pid_is_alive(-1));
        // A very high pid is almost certainly unused.
        assert!(!pid_is_alive(2_000_000_000));
    }

    #[test]
    fn read_pidfile_roundtrip_and_absent() {
        let dir = std::env::temp_dir().join(format!("loom-chromium-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read_pidfile(&dir), None); // no pidfile yet
        std::fs::write(dir.join(PIDFILE_NAME), "4242").unwrap();
        assert_eq!(read_pidfile(&dir), Some(4242));
        std::fs::write(dir.join(PIDFILE_NAME), "garbage").unwrap();
        assert_eq!(read_pidfile(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_skips_symlinks_and_foreign_dirs() {
        let root = std::env::temp_dir().join(format!("reaper-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(root.join("loom-chromium-good01")).unwrap();
        std::fs::create_dir_all(root.join("not-loom-thing")).unwrap(); // ignored (no prefix)
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            root.join("loom-chromium-good01"),
            root.join("loom-chromium-link0"),
        )
        .unwrap();

        let entries = scan_browser_dirs(&root);
        let ids: Vec<_> = entries.iter().map(|e| e.session_id.as_str()).collect();
        assert!(ids.contains(&"good01"));
        assert!(!ids.iter().any(|id| id.starts_with("not-loom")));
        // The symlink entry is present but flagged so classify_orphans rejects it.
        let link = entries.iter().find(|e| e.session_id == "link0");
        assert!(link.map(|e| e.is_symlink).unwrap_or(true));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn terminate_group_kills_a_real_child_tree() {
        // Spawn a child in its OWN process group so killpg(child) reaps just it.
        let mut child = Command::new("sh")
            .args(["-c", "sleep 30"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0) // child pid == its pgid
            .spawn()
            .expect("spawn sleep");
        let pid = child.id() as i32;
        assert!(pid_is_alive(pid));
        let outcome = terminate_group(pid, Duration::from_millis(500));
        assert_eq!(outcome, KillOutcome::Terminated);
        // Reap the zombie and confirm it actually exited.
        let _ = child.wait();
        assert_eq!(
            terminate_group(pid, Duration::from_millis(50)),
            KillOutcome::AlreadyDead
        );
    }
}
