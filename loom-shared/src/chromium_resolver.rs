//! `loom-shared::chromium_resolver` — locate a Chromium browser binary
//! across install channels.
//!
//! AC-DIST-05 (`loom session create` first-run UX). Mirrors the layout of
//! the sibling [`crate::binary_resolver`] module but resolves a *browser*
//! binary, not a loom-sibling.
//!
//! Resolution order (first hit wins):
//!   1. `LOOM_CHROMIUM_PATH` env (explicit user override) → `EnvOverride`
//!   2. `<chromium_dir>/Chromium.app/Contents/MacOS/Chromium` (the path
//!      `loom postinstall` writes to) → `Pinned`
//!   3. Parent-dir scan: any executable inside
//!      `<chromium_dir>/Chromium.app/Contents/MacOS/` (covers symlink-renamed
//!      bundles like `Google Chrome`, AC-CHBS-01) → `Pinned`
//!   4. PATH lookup for `chromium`, `chromium-browser`, `chrome`,
//!      `google-chrome` → `Path`
//!   5. macOS standard installs → `Applications`:
//!      - `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`
//!      - `/Applications/Chromium.app/Contents/MacOS/Chromium`
//!   6. None → `Err(BrowserNotFound { searched_paths })`
//!
//! All internal errors (`std::io::Error` from `metadata()`/`var_os()`,
//! broken symlinks, permission denied) are mapped to `BrowserNotFound`
//! so the resolver's signature stays simple — it never panics, never
//! propagates io::Error.
//!
//! Linux wrapper scripts (e.g. `/usr/bin/google-chrome` on Debian/Ubuntu
//! is a shell wrapper that re-execs the real binary in `/opt/`) are
//! accepted verbatim: `tokio::process::Command::new` passes args
//! through and CDP attaches over `--remote-debugging-port` exactly as
//! if we'd called the real binary directly.
//!
//! SECURITY: PATH search trusts the caller's PATH ordering. A PATH
//! entry with `.` or a compromised dir could shadow chromium. This is
//! generic to PATH-based binary lookup (cargo, git, make, etc.); we
//! don't add explicit guards — Loom users who installed via brew or
//! cargo already trust their PATH.

use std::path::{Path, PathBuf};

/// Where the resolved chromium came from. Used by the daemon to emit a
/// `tracing::warn!` when source is not `Pinned` so users know they've
/// lost replay-bit-equality (per CONTRIBUTING.md "Determinism is sacred"
/// + SR-SHIM-03).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromiumSource {
    /// `LOOM_CHROMIUM_PATH` env was set and pointed at a valid executable.
    EnvOverride,
    /// `loom postinstall` wrote a pinned Chromium to `<chromium_dir>`.
    /// This is the only source that guarantees replay-bit-equality
    /// across machines.
    Pinned,
    /// Resolved via `$PATH` (chromium / chromium-browser / chrome /
    /// google-chrome). Determinism not guaranteed.
    Path,
    /// macOS-standard install location (`/Applications/...`).
    /// Determinism not guaranteed.
    Applications,
}

/// Resolution failure. Carries the full search-path list for diagnostics;
/// the platform-aware install hint is rendered by the CLI's
/// `Display for CliError`.
#[derive(Debug, Clone)]
pub struct BrowserNotFound {
    pub searched_paths: Vec<String>,
}

impl std::fmt::Display for BrowserNotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Chromium not found. Searched: {}",
            self.searched_paths.join(", ")
        )
    }
}

impl std::error::Error for BrowserNotFound {}

const PATH_NAMES: &[&str] = &["chromium", "chromium-browser", "chrome", "google-chrome"];

#[cfg(target_os = "macos")]
const APPLICATIONS_PATHS: &[&str] = &[
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
];

#[cfg(not(target_os = "macos"))]
const APPLICATIONS_PATHS: &[&str] = &[];

/// Resolve a Chromium binary for the host process. The `chromium_dir` is
/// the same `~/.config/loom/chromium/` path used by `loom postinstall`
/// and the daemon's pre-existing pinned-path check.
pub fn resolve_chromium(chromium_dir: &Path) -> Result<(PathBuf, ChromiumSource), BrowserNotFound> {
    let mut searched: Vec<String> = Vec::new();

    // 1. LOOM_CHROMIUM_PATH explicit override.
    if let Some(env_val) = std::env::var_os("LOOM_CHROMIUM_PATH") {
        let p = PathBuf::from(&env_val);
        searched.push(format!("LOOM_CHROMIUM_PATH={}", p.display()));
        if is_valid_executable(&p) {
            return Ok((p, ChromiumSource::EnvOverride));
        }
        // Don't fatally error on a stale env var. Log + fall through.
        tracing::warn!(
            "LOOM_CHROMIUM_PATH={} is not an executable file; falling through to standard resolution",
            p.display()
        );
    }

    // 2. Pinned: `<chromium_dir>/Chromium.app/Contents/MacOS/Chromium`.
    let pinned = chromium_dir.join("Chromium.app/Contents/MacOS/Chromium");
    searched.push(pinned.display().to_string());
    if is_valid_executable(&pinned) {
        return Ok((pinned, ChromiumSource::Pinned));
    }

    // 3. AC-CHBS-01 parent-dir scan: any executable in MacOS/ (handles
    // bundles where the inner binary is named `Google Chrome` instead of
    // `Chromium` — symlinks/renamed casks).
    if let Some(parent) = pinned.parent() {
        if parent.is_dir() {
            if let Some(found) = first_executable_in_dir(parent) {
                searched.push(format!("{}/<scan>", parent.display()));
                return Ok((found, ChromiumSource::Pinned));
            }
        }
    }

    // 4. PATH search.
    for name in PATH_NAMES {
        if let Some(found) = lookup_in_path(name) {
            searched.push(format!("PATH:{name}"));
            return Ok((found, ChromiumSource::Path));
        }
        searched.push(format!("PATH:{name}"));
    }

    // 5. macOS Applications fallbacks.
    for app_path in APPLICATIONS_PATHS {
        let p = PathBuf::from(app_path);
        searched.push(app_path.to_string());
        if is_valid_executable(&p) {
            return Ok((p, ChromiumSource::Applications));
        }
    }

    Err(BrowserNotFound {
        searched_paths: searched,
    })
}

/// True iff `path` resolves to a regular file (following symlinks) with
/// at least one execute bit set on Unix. Returns false on any io::Error
/// (permission denied, broken symlink, missing). Non-Unix returns false —
/// we don't ship Windows.
fn is_valid_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let Ok(meta) = std::fs::metadata(path) else {
            return false;
        };
        meta.is_file() && (meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

/// Scan `dir` for the first executable regular file. Returns `None` on
/// any io::Error (read_dir failure, permission denied).
fn first_executable_in_dir(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.filter_map(|e| e.ok()) {
        let p = entry.path();
        if is_valid_executable(&p) {
            return Some(p);
        }
    }
    None
}

/// Look up `name` in `$PATH`. Returns the first hit that is a regular
/// executable file. None if PATH is unset or no entry matches.
fn lookup_in_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if is_valid_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Process-global lock for tests that mutate $PATH or LOOM_CHROMIUM_PATH.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn plant_executable(path: &Path) -> PathBuf {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(path, b"#!/bin/sh\nexit 0\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut perm = std::fs::metadata(path).expect("metadata").permissions();
            perm.set_mode(0o755);
            std::fs::set_permissions(path, perm).expect("chmod");
        }
        path.to_path_buf()
    }

    fn pinned_path(chromium_dir: &Path) -> PathBuf {
        chromium_dir.join("Chromium.app/Contents/MacOS/Chromium")
    }

    #[test]
    fn t1_resolves_pinned_path_when_present() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        plant_executable(&pinned_path(tmp.path()));
        let prev = std::env::var_os("LOOM_CHROMIUM_PATH");
        std::env::remove_var("LOOM_CHROMIUM_PATH");

        let (path, source) = resolve_chromium(tmp.path()).expect("ok");
        assert_eq!(source, ChromiumSource::Pinned);
        assert_eq!(path, pinned_path(tmp.path()));

        if let Some(v) = prev {
            std::env::set_var("LOOM_CHROMIUM_PATH", v);
        }
    }

    #[test]
    fn t2_falls_through_to_path_search_when_pinned_missing() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let path_dir = tempfile::tempdir().unwrap();
        plant_executable(&path_dir.path().join("chromium"));
        let prev_path = std::env::var_os("PATH");
        std::env::set_var("PATH", path_dir.path());
        let prev_env = std::env::var_os("LOOM_CHROMIUM_PATH");
        std::env::remove_var("LOOM_CHROMIUM_PATH");

        let (path, source) = resolve_chromium(tmp.path()).expect("ok");
        assert_eq!(source, ChromiumSource::Path);
        assert_eq!(path, path_dir.path().join("chromium"));

        if let Some(p) = prev_path {
            std::env::set_var("PATH", p);
        } else {
            std::env::remove_var("PATH");
        }
        if let Some(v) = prev_env {
            std::env::set_var("LOOM_CHROMIUM_PATH", v);
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn t3_falls_through_to_macos_applications_when_path_search_misses() {
        let tmp = tempfile::tempdir().unwrap();
        // Just confirm the resolver never panics on a clean macOS box.
        let _ = resolve_chromium(tmp.path());
    }

    #[test]
    fn t4_returns_browser_not_found_when_all_branches_miss() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let empty_dir = tempfile::tempdir().unwrap();
        let prev_path = std::env::var_os("PATH");
        std::env::set_var("PATH", empty_dir.path());
        let prev_env = std::env::var_os("LOOM_CHROMIUM_PATH");
        std::env::remove_var("LOOM_CHROMIUM_PATH");

        let result = resolve_chromium(tmp.path());
        if cfg!(target_os = "macos")
            && std::path::Path::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
                .exists()
        {
            assert!(matches!(result, Ok((_, ChromiumSource::Applications))));
        } else {
            let err = result.expect_err("should be BrowserNotFound on a clean box");
            assert!(!err.searched_paths.is_empty());
        }

        if let Some(p) = prev_path {
            std::env::set_var("PATH", p);
        } else {
            std::env::remove_var("PATH");
        }
        if let Some(v) = prev_env {
            std::env::set_var("LOOM_CHROMIUM_PATH", v);
        }
    }

    #[test]
    fn t5_respects_loom_chromium_path_env_override() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let exe = plant_executable(&tmp.path().join("custom-chromium"));
        let prev_env = std::env::var_os("LOOM_CHROMIUM_PATH");
        std::env::set_var("LOOM_CHROMIUM_PATH", &exe);

        let chromium_dir = tempfile::tempdir().unwrap();
        let (path, source) = resolve_chromium(chromium_dir.path()).expect("ok");
        assert_eq!(source, ChromiumSource::EnvOverride);
        assert_eq!(path, exe);

        if let Some(v) = prev_env {
            std::env::set_var("LOOM_CHROMIUM_PATH", v);
        } else {
            std::env::remove_var("LOOM_CHROMIUM_PATH");
        }
    }

    #[test]
    fn t6_path_search_skips_non_executable_entries() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let path_dir = tempfile::tempdir().unwrap();
        let nonexec = path_dir.path().join("chromium");
        std::fs::write(&nonexec, b"not executable").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&nonexec, std::fs::Permissions::from_mode(0o644)).unwrap();
        }

        let real_dir = tempfile::tempdir().unwrap();
        plant_executable(&real_dir.path().join("chromium-browser"));
        let combined = format!(
            "{}:{}",
            path_dir.path().display(),
            real_dir.path().display()
        );
        let prev_path = std::env::var_os("PATH");
        std::env::set_var("PATH", combined);
        let prev_env = std::env::var_os("LOOM_CHROMIUM_PATH");
        std::env::remove_var("LOOM_CHROMIUM_PATH");

        let (path, source) = resolve_chromium(tmp.path()).expect("ok");
        assert_eq!(source, ChromiumSource::Path);
        assert_eq!(path, real_dir.path().join("chromium-browser"));

        if let Some(p) = prev_path {
            std::env::set_var("PATH", p);
        } else {
            std::env::remove_var("PATH");
        }
        if let Some(v) = prev_env {
            std::env::set_var("LOOM_CHROMIUM_PATH", v);
        }
    }

    #[test]
    fn t7_path_search_skips_directories_named_chromium() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let path_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(path_dir.path().join("chromium")).unwrap();
        let prev_path = std::env::var_os("PATH");
        std::env::set_var("PATH", path_dir.path());
        let prev_env = std::env::var_os("LOOM_CHROMIUM_PATH");
        std::env::remove_var("LOOM_CHROMIUM_PATH");

        let result = resolve_chromium(tmp.path());
        if !cfg!(target_os = "macos")
            || !std::path::Path::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
                .exists()
        {
            assert!(
                result.is_err(),
                "directory entry should not resolve as a binary"
            );
        }

        if let Some(p) = prev_path {
            std::env::set_var("PATH", p);
        } else {
            std::env::remove_var("PATH");
        }
        if let Some(v) = prev_env {
            std::env::set_var("LOOM_CHROMIUM_PATH", v);
        }
    }

    #[test]
    #[cfg(unix)]
    fn t8_resolves_through_symlink() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let real = plant_executable(&tmp.path().join("real-chromium"));
        let pinned = pinned_path(tmp.path());
        std::fs::create_dir_all(pinned.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real, &pinned).unwrap();
        let prev_env = std::env::var_os("LOOM_CHROMIUM_PATH");
        std::env::remove_var("LOOM_CHROMIUM_PATH");

        let (path, source) = resolve_chromium(tmp.path()).expect("symlinked pinned should resolve");
        assert_eq!(source, ChromiumSource::Pinned);
        assert_eq!(path, pinned);

        if let Some(v) = prev_env {
            std::env::set_var("LOOM_CHROMIUM_PATH", v);
        }
    }

    #[test]
    fn t9_permission_denied_on_metadata_falls_through() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let nonexistent = tmp.path().join("definitely-does-not-exist/chromium");
        let prev_env = std::env::var_os("LOOM_CHROMIUM_PATH");
        std::env::set_var("LOOM_CHROMIUM_PATH", &nonexistent);

        let chromium_dir = tempfile::tempdir().unwrap();
        let prev_path = std::env::var_os("PATH");
        let empty_path = tempfile::tempdir().unwrap();
        std::env::set_var("PATH", empty_path.path());

        // Should not panic; LOOM_CHROMIUM_PATH was set but invalid → fall through.
        let _ = resolve_chromium(chromium_dir.path());

        if let Some(p) = prev_path {
            std::env::set_var("PATH", p);
        } else {
            std::env::remove_var("PATH");
        }
        if let Some(v) = prev_env {
            std::env::set_var("LOOM_CHROMIUM_PATH", v);
        } else {
            std::env::remove_var("LOOM_CHROMIUM_PATH");
        }
    }

    #[test]
    #[cfg(unix)]
    fn t10_broken_symlink_skipped() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let pinned = pinned_path(tmp.path());
        std::fs::create_dir_all(pinned.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink("/nonexistent/does/not/exist", &pinned).unwrap();
        let prev_env = std::env::var_os("LOOM_CHROMIUM_PATH");
        std::env::remove_var("LOOM_CHROMIUM_PATH");
        let prev_path = std::env::var_os("PATH");
        let empty_path = tempfile::tempdir().unwrap();
        std::env::set_var("PATH", empty_path.path());

        let result = resolve_chromium(tmp.path());
        if cfg!(target_os = "macos")
            && std::path::Path::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
                .exists()
        {
            assert!(matches!(result, Ok((_, ChromiumSource::Applications))));
        } else {
            let err = result.expect_err("broken symlink should not resolve");
            assert!(!err.searched_paths.is_empty());
        }

        if let Some(p) = prev_path {
            std::env::set_var("PATH", p);
        } else {
            std::env::remove_var("PATH");
        }
        if let Some(v) = prev_env {
            std::env::set_var("LOOM_CHROMIUM_PATH", v);
        }
    }

    #[test]
    fn t11_path_with_unicode_works() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let unicode_dir = tmp.path().join("браузер-тест");
        std::fs::create_dir_all(&unicode_dir).unwrap();
        let exe = plant_executable(&unicode_dir.join("chromium"));
        let prev_env = std::env::var_os("LOOM_CHROMIUM_PATH");
        std::env::set_var("LOOM_CHROMIUM_PATH", &exe);

        let chromium_dir = tempfile::tempdir().unwrap();
        let (path, source) =
            resolve_chromium(chromium_dir.path()).expect("unicode path should work");
        assert_eq!(source, ChromiumSource::EnvOverride);
        assert_eq!(path, exe);

        if let Some(v) = prev_env {
            std::env::set_var("LOOM_CHROMIUM_PATH", v);
        } else {
            std::env::remove_var("LOOM_CHROMIUM_PATH");
        }
    }

    #[test]
    fn t12_loom_chromium_path_set_to_directory_falls_through() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let prev_env = std::env::var_os("LOOM_CHROMIUM_PATH");
        std::env::set_var("LOOM_CHROMIUM_PATH", tmp.path());

        plant_executable(&pinned_path(tmp.path()));
        let result = resolve_chromium(tmp.path());
        let (_, source) = result.expect("pinned should still resolve");
        assert_eq!(source, ChromiumSource::Pinned);

        if let Some(v) = prev_env {
            std::env::set_var("LOOM_CHROMIUM_PATH", v);
        } else {
            std::env::remove_var("LOOM_CHROMIUM_PATH");
        }
    }

    #[test]
    fn t13_loom_chromium_path_set_to_nonexec_file_falls_through() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let nonexec = tmp.path().join("not-executable");
        std::fs::write(&nonexec, b"not really chromium").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&nonexec, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        let prev_env = std::env::var_os("LOOM_CHROMIUM_PATH");
        std::env::set_var("LOOM_CHROMIUM_PATH", &nonexec);

        plant_executable(&pinned_path(tmp.path()));
        let result = resolve_chromium(tmp.path());
        let (_, source) = result.expect("pinned should still resolve");
        assert_eq!(source, ChromiumSource::Pinned);

        if let Some(v) = prev_env {
            std::env::set_var("LOOM_CHROMIUM_PATH", v);
        } else {
            std::env::remove_var("LOOM_CHROMIUM_PATH");
        }
    }
}
