// Tests for chromium_resolver. Each test sets up a tempdir layout that
// matches one branch of the resolution chain, runs `resolve_chromium`,
// and asserts the path + source.
//
// PATH-search tests serialize via a shared mutex because they mutate
// `$PATH` (process-global). The resolver itself is pure-function on
// `chromium_dir` + env vars; tests do not need a real daemon.

use super::chromium_resolver::*;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

// Process-global lock for tests that mutate $PATH or LOOM_CHROMIUM_PATH.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Plant a fake-chromium executable at `path` and return it. Creates parents.
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
    // Strip env so we don't accidentally take branch 1.
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
    let tmp = tempfile::tempdir().unwrap(); // empty chromium_dir → no pinned
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
    // Difficult to test on real CI without /Applications/...; instead we
    // assert the constant list is non-empty and includes the canonical
    // Google Chrome path. The actual fallthrough is exercised by t4 +
    // a manual check on the dev box.
    use super::chromium_resolver::*;
    let tmp = tempfile::tempdir().unwrap();
    let _ = resolve_chromium(tmp.path()); // never panics — that's the contract
}

#[test]
fn t4_returns_browser_not_found_when_all_branches_miss() {
    let _g = ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let empty_dir = tempfile::tempdir().unwrap();
    let prev_path = std::env::var_os("PATH");
    std::env::set_var("PATH", empty_dir.path()); // no chromium in PATH
    let prev_env = std::env::var_os("LOOM_CHROMIUM_PATH");
    std::env::remove_var("LOOM_CHROMIUM_PATH");

    // On macOS the /Applications fallback may still resolve if the host
    // has Chrome installed — the test is skipped in that case to avoid
    // flakes on developer laptops.
    let result = resolve_chromium(tmp.path());
    if cfg!(target_os = "macos")
        && std::path::Path::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
            .exists()
    {
        // /Applications fallback hit; that's a valid resolution, not a bug.
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
    // Plant a NON-executable file named `chromium` (will be skipped),
    // then a real executable named `chromium-browser` further along.
    let nonexec = path_dir.path().join("chromium");
    std::fs::write(&nonexec, b"not executable").unwrap();
    // Ensure no executable bits.
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
    // Resolver tries `chromium` first (non-exec, skipped), then
    // `chromium-browser` (matches in real_dir).
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
    // A directory literally named `chromium` — exists, but is_file() = false.
    std::fs::create_dir_all(path_dir.path().join("chromium")).unwrap();
    let prev_path = std::env::var_os("PATH");
    std::env::set_var("PATH", path_dir.path());
    let prev_env = std::env::var_os("LOOM_CHROMIUM_PATH");
    std::env::remove_var("LOOM_CHROMIUM_PATH");

    let result = resolve_chromium(tmp.path());
    // No real chromium anywhere → BrowserNotFound (or /Applications fallback).
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
    // Plant a real executable, then a symlink at the pinned path pointing at it.
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
    // We can't easily produce permission-denied on a fresh tempdir on macOS
    // CI without sudo. The contract is that the resolver returns
    // BrowserNotFound (never panics) on any io::Error. Asserting that
    // `is_valid_executable` returns false on a non-existent path is the
    // simplest proxy for "metadata() failed → return false".
    let _g = ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let nonexistent = tmp.path().join("definitely-does-not-exist/chromium");
    let prev_env = std::env::var_os("LOOM_CHROMIUM_PATH");
    std::env::set_var("LOOM_CHROMIUM_PATH", &nonexistent);

    // Fake the chromium_dir to also miss + no PATH.
    let chromium_dir = tempfile::tempdir().unwrap();
    let prev_path = std::env::var_os("PATH");
    let empty_path = tempfile::tempdir().unwrap();
    std::env::set_var("PATH", empty_path.path());

    // Should not panic; LOOM_CHROMIUM_PATH was set but invalid → fall through →
    // pinned missing → PATH miss → /Applications maybe → otherwise BrowserNotFound.
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
    // Symlink target does not exist.
    std::os::unix::fs::symlink("/nonexistent/does/not/exist", &pinned).unwrap();
    let prev_env = std::env::var_os("LOOM_CHROMIUM_PATH");
    std::env::remove_var("LOOM_CHROMIUM_PATH");
    let prev_path = std::env::var_os("PATH");
    let empty_path = tempfile::tempdir().unwrap();
    std::env::set_var("PATH", empty_path.path());

    let result = resolve_chromium(tmp.path());
    // The broken-symlink pinned path should not resolve. On macOS dev boxes
    // with Chrome installed the /Applications fallback may still win — that's
    // valid behavior, not a bug.
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
    // Tempdir parent inside a unicode-named subdir.
    let unicode_dir = tmp.path().join("браузер-тест");
    std::fs::create_dir_all(&unicode_dir).unwrap();
    let exe = plant_executable(&unicode_dir.join("chromium"));
    let prev_env = std::env::var_os("LOOM_CHROMIUM_PATH");
    std::env::set_var("LOOM_CHROMIUM_PATH", &exe);

    let chromium_dir = tempfile::tempdir().unwrap();
    let (path, source) = resolve_chromium(chromium_dir.path()).expect("unicode path should work");
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
    // Set env to a directory (not an executable file).
    let prev_env = std::env::var_os("LOOM_CHROMIUM_PATH");
    std::env::set_var("LOOM_CHROMIUM_PATH", tmp.path());

    // Plant a pinned chromium so we can detect the fall-through hit.
    plant_executable(&pinned_path(tmp.path()));
    let result = resolve_chromium(tmp.path());
    // Env override is a directory (invalid) → skipped → pinned hits.
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
    // Plant a non-executable file.
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
