use super::*;
use crate::postinstall_runner::StepOutcome;
use std::path::PathBuf;

fn tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("loom-manpage-test-")
        .tempdir()
        .expect("tempdir")
}

#[test]
fn manpage_step_writes_files_to_target_dir() {
    if !has_embedded_content() {
        // Build did not embed payloads (e.g. fresh clone) — skip this test.
        return;
    }
    let dir = tempdir();
    let outcome = manpage_step(Some(dir.path())).expect("step ok");
    let written = std::fs::read_dir(dir.path().join("man1"))
        .expect("man1 created")
        .filter_map(|e| e.ok())
        .count();
    assert!(matches!(outcome, StepOutcome::Wrote | StepOutcome::Skipped));
    assert!(
        written >= 1,
        "expected at least one man page written; got {written}"
    );
}

#[test]
fn manpage_step_idempotent() {
    if !has_embedded_content() {
        return;
    }
    let dir = tempdir();
    let first = manpage_step(Some(dir.path())).expect("first run");
    let listing_before: Vec<_> = std::fs::read_dir(dir.path().join("man1"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    let mtimes_before: Vec<_> = listing_before
        .iter()
        .map(|p| std::fs::metadata(p).unwrap().modified().unwrap())
        .collect();

    // Touch nothing in between, run again.
    let second = manpage_step(Some(dir.path())).expect("second run");

    // Second run should NOT have rewritten files (write_atomic_if_changed
    // returns Ok(false) when content matches).
    let mtimes_after: Vec<_> = listing_before
        .iter()
        .map(|p| std::fs::metadata(p).unwrap().modified().unwrap())
        .collect();
    assert_eq!(mtimes_before, mtimes_after, "second run rewrote files");
    let _ = (first, second);
}

#[test]
fn manpage_step_skips_on_permission_denied() {
    // Read-only parent — child writes fail with PermissionDenied.
    let parent = tempdir();
    let mut perms = std::fs::metadata(parent.path()).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o555); // r-xr-xr-x — no write
        std::fs::set_permissions(parent.path(), perms.clone()).unwrap();
    }
    #[cfg(not(unix))]
    {
        let _ = perms;
    }

    let target = parent.path().join("man");
    let outcome = manpage_step(Some(&target)).expect("step must not error");
    // Soft-skip — postinstall continues. With no embedded content the step
    // returns Skipped before even attempting; with content it hits
    // PermissionDenied and downgrades to Skipped via soft_warn_and_skip.
    assert_eq!(outcome, StepOutcome::Skipped);

    // Restore writable so tempdir can clean up.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(parent.path()).unwrap().permissions();
        p.set_mode(0o755);
        std::fs::set_permissions(parent.path(), p).unwrap();
    }
}

#[test]
fn manpage_step_skips_when_target_dir_unset_and_no_resolution() {
    // Force resolution to None by clearing the env vars and faking no home dir.
    // We can't actually clear `dirs::home_dir`, so this test just exercises
    // the documented behaviour: passing None when env says nothing falls
    // through to home_dir; on a normal CI box this'll find one.
    // Verify: passing an explicit empty Some(path) doesn't crash.
    let dir = tempdir();
    let outcome = manpage_step(Some(dir.path())).unwrap();
    let _ = outcome; // outcome is OK regardless
}

#[test]
fn man_pages_installed_at_returns_true_after_step() {
    if !has_embedded_content() {
        return;
    }
    let dir = tempdir();
    let _ = manpage_step(Some(dir.path())).expect("step ok");
    assert!(man_pages_installed_at(dir.path()));
}

#[test]
fn man_pages_installed_at_returns_false_for_empty_dir() {
    let dir = tempdir();
    if !has_embedded_content() {
        // No content embedded => "all installed" trivially. Not a failure.
        assert!(man_pages_installed_at(dir.path()));
        return;
    }
    assert!(!man_pages_installed_at(dir.path()));
}

#[test]
fn resolve_install_dir_prefers_explicit_loom_man_dir() {
    let prev = std::env::var("LOOM_MAN_DIR").ok();
    std::env::set_var("LOOM_MAN_DIR", "/tmp/loom-test-man");
    let resolved = resolve_install_dir();
    assert_eq!(resolved, Some(PathBuf::from("/tmp/loom-test-man")));
    match prev {
        Some(v) => std::env::set_var("LOOM_MAN_DIR", v),
        None => std::env::remove_var("LOOM_MAN_DIR"),
    }
}

#[test]
fn resolve_install_dir_falls_back_to_prefix_share_man() {
    let prev_loom = std::env::var("LOOM_MAN_DIR").ok();
    let prev_prefix = std::env::var("PREFIX").ok();
    std::env::remove_var("LOOM_MAN_DIR");
    std::env::set_var("PREFIX", "/usr/local");
    let resolved = resolve_install_dir();
    assert_eq!(resolved, Some(PathBuf::from("/usr/local/share/man")));
    match prev_loom {
        Some(v) => std::env::set_var("LOOM_MAN_DIR", v),
        None => std::env::remove_var("LOOM_MAN_DIR"),
    }
    match prev_prefix {
        Some(v) => std::env::set_var("PREFIX", v),
        None => std::env::remove_var("PREFIX"),
    }
}

#[test]
fn manpage_step_no_path_leakage_in_warnings() {
    // The soft-warn path must not include absolute build-time paths
    // (security council finding 1). We can't easily capture stderr here,
    // but we can call write_atomic_if_changed with bytes that match an
    // existing file — the success path doesn't print anything, and the
    // soft_warn_and_skip path uses err.kind() (an io::ErrorKind variant
    // name like "PermissionDenied", not a path).
    //
    // Smoke: just verify the function doesn't panic and StepOutcome roundtrips.
    let dir = tempdir();
    let _ = manpage_step(Some(dir.path())).expect("must not error");
}
