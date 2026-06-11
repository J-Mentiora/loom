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

/// True iff process group `pgid` has ANY live member (`kill(-pgid, 0)` succeeds or fails
/// with `EPERM`). The leader pid alone is NOT a group-liveness signal: Chromium's leader can
/// exit while GPU/renderer/utility members survive — the classic leaked tree this reaper
/// exists for. POSIX keeps the pgid valid while any member lives, so signalling the GROUP
/// probes exactly the set `killpg` would hit. Falls back to the leader-pid probe on
/// EINVAL/unexpected errnos (fail-safe: never report a group dead on a probe error).
pub fn group_is_alive(pgid: i32) -> bool {
    if pgid <= 0 {
        return false;
    }
    // SAFETY: kill with signal 0 and a negative pid performs error checking against the
    // process group without delivering a signal; pgid is validated > 0.
    let rc = unsafe { libc::kill(-pgid, 0) };
    if rc == 0 {
        return true;
    }
    match std::io::Error::last_os_error().raw_os_error() {
        // EPERM = at least one member exists that we may not signal — alive.
        Some(libc::EPERM) => true,
        Some(libc::ESRCH) => false,
        // Probe failed in an unexpected way — fall back to the leader pid.
        _ => pid_is_alive(pgid),
    }
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

/// True iff ANY member of process group `pgid` has `needle` (the user-data-dir path) in its
/// command line. The leader-pid fast path covers the common case; when the leader is already
/// gone (dead-leader tree) the surviving members are enumerated, so C3 verification can still
/// succeed and the tree remains killable. Fail-safe: returns `false` on any read error.
pub fn group_cmdline_contains(pgid: i32, needle: &str) -> bool {
    if pgid <= 0 {
        return false;
    }
    if pid_cmdline_contains(pgid, needle) {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        // Enumerate /proc for members of the group: /proc/<pid>/stat field 5 is pgrp, but
        // field 2 (comm) may contain spaces/parens — split after the LAST ')'.
        let Ok(rd) = std::fs::read_dir("/proc") else {
            return false;
        };
        for entry in rd.flatten() {
            let name = entry.file_name();
            let Some(pid_str) = name.to_str() else {
                continue;
            };
            let Ok(pid) = pid_str.parse::<i32>() else {
                continue;
            };
            if pid == pgid {
                continue; // leader already checked above
            }
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                continue;
            };
            let Some((_, rest)) = stat.rsplit_once(')') else {
                continue;
            };
            // rest = " <state> <ppid> <pgrp> <session> ..."
            let mut fields = rest.split_whitespace();
            let _state = fields.next();
            let _ppid = fields.next();
            let Some(pgrp) = fields.next().and_then(|s| s.parse::<i32>().ok()) else {
                continue;
            };
            if pgrp == pgid && pid_cmdline_contains(pid, needle) {
                return true;
            }
        }
        false
    }
    #[cfg(not(target_os = "linux"))]
    {
        // macOS / BSD: list every process's pgid + full command, match the group's members.
        match std::process::Command::new("ps")
            .args(["-axo", "pgid=,command="])
            .output()
        {
            Ok(out) if out.status.success() => {
                let text = String::from_utf8_lossy(&out.stdout);
                text.lines().any(|line| {
                    let trimmed = line.trim_start();
                    let Some((pg, cmd)) = trimmed.split_once(|c: char| c.is_whitespace()) else {
                        return false;
                    };
                    pg.parse::<i32>().ok() == Some(pgid) && cmd.contains(needle)
                })
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
/// `process_group(0)`). Liveness is probed against the GROUP (`group_is_alive`), not the
/// leader pid: a dead leader with surviving renderer/GPU members must still be signalled,
/// and the grace poll must not declare victory while stragglers remain.
/// Blocking — callers in async context wrap this in `spawn_blocking`.
pub fn terminate_group(pgid: i32, grace: Duration) -> KillOutcome {
    if pgid <= 0 || !group_is_alive(pgid) {
        return KillOutcome::AlreadyDead;
    }
    // SAFETY: killpg delivers a signal to every process in the group; pgid is validated > 0.
    unsafe { libc::killpg(pgid, libc::SIGTERM) };

    let deadline = std::time::Instant::now() + grace;
    let poll = Duration::from_millis(50);
    while std::time::Instant::now() < deadline {
        if !group_is_alive(pgid) {
            return KillOutcome::Terminated;
        }
        std::thread::sleep(poll);
    }
    if group_is_alive(pgid) {
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
/// Returns true iff the dir is gone afterwards (removed here, or already absent) — so the
/// sweep report only counts dirs that were actually reclaimed.
pub fn remove_orphan_dir(session_id: &str, path: &Path) -> bool {
    if !is_safe_session_id(session_id) {
        tracing::warn!(session = %session_id, "reaper: refusing to remove unsafe profile dir");
        return false;
    }
    match std::fs::remove_dir_all(path) {
        Ok(()) => {
            tracing::debug!(dir = %path.display(), "reaper: removed orphan profile dir");
            true
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => {
            tracing::warn!(dir = %path.display(), error = %e, "reaper: orphan dir cleanup failed");
            false
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

    /// Spawn a leader (own process group) that backgrounds `cmd` and exits immediately,
    /// leaving a surviving member in the group with a dead, reaped leader.
    fn spawn_dead_leader_group(cmd: &str) -> i32 {
        let mut child = Command::new("sh")
            .args(["-c", &format!("{cmd} & exit 0")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0) // leader pid == pgid
            .spawn()
            .expect("spawn group leader");
        let pgid = child.id() as i32;
        let _ = child.wait(); // leader exits + is reaped; bg member keeps the pgid alive
        pgid
    }

    /// The dead-leader tree: kill(leader, 0) says dead, but the GROUP still has a live
    /// member. group_is_alive must see it and terminate_group must actually kill it —
    /// previously the leader-pid probe returned AlreadyDead and no signal was ever sent.
    #[test]
    fn group_is_alive_and_terminate_see_survivors_of_a_dead_leader() {
        let pgid = spawn_dead_leader_group("sleep 30");
        assert!(
            !pid_is_alive(pgid),
            "leader must be dead+reaped for this scenario"
        );
        assert!(
            group_is_alive(pgid),
            "group must read alive via the surviving member"
        );
        assert_eq!(
            terminate_group(pgid, Duration::from_millis(1000)),
            KillOutcome::Terminated,
            "dead-leader tree must still be signalled, not reported AlreadyDead"
        );
        assert!(!group_is_alive(pgid), "survivor must be gone after killpg");
    }

    #[test]
    fn group_is_alive_false_for_bogus_and_dead_groups() {
        assert!(!group_is_alive(0));
        assert!(!group_is_alive(-1));
        assert!(!group_is_alive(2_000_000_000));
    }

    /// C3 verification across group members: the leader is dead, but a surviving member's
    /// command line carries the needle — group_cmdline_contains must find it (and must NOT
    /// match a needle that no member carries).
    #[test]
    fn group_cmdline_contains_matches_surviving_member_of_dead_leader() {
        let needle = format!("user-data-dir=/tmp/reaper-needle-{}", std::process::id());
        // Two commands in the inner -c string prevent the shell's exec optimization, so the
        // surviving inner sh keeps the needle-bearing string in its argv.
        let pgid = spawn_dead_leader_group(&format!("sh -c 'sleep 30; true # {needle}'"));
        assert!(!pid_is_alive(pgid), "leader must be dead");
        assert!(group_is_alive(pgid), "member must be alive");
        assert!(
            group_cmdline_contains(pgid, &needle),
            "needle carried by a surviving member must verify"
        );
        assert!(
            !group_cmdline_contains(pgid, "no-such-needle-anywhere"),
            "fail-safe: absent needle must not verify"
        );
        assert_eq!(
            terminate_group(pgid, Duration::from_millis(1000)),
            KillOutcome::Terminated
        );
    }
}
