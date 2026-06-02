//! Security-acceptance tests for the `web.set_input_files` upload-path allow-list.
//!
//! Targets the pure daemon-side validator (`upload_guard::validate_upload_paths`)
//! that `dispatch_action_blocking` runs BEFORE dispatching to the wasm guest —
//! the authoritative security gate (plan-council FND#5: most-trusted, earliest
//! component). Written test-first (RED) before `crate::upload_guard` existed.
//!
//! Security contract (decisions.md D-SEC-1/2, D-CAPS-1):
//!   - unset LOOM_UPLOAD_ROOT       → fail closed (RootNotConfigured)
//!   - path outside root            → PathBlocked   (enforced in ALL profiles)
//!   - symlink escaping root        → PathBlocked   (std::fs::canonicalize)
//!   - non-existent / non-file      → PathNotFound
//!   - > MAX_UPLOAD_FILES           → TooManyFiles
//!   - file > MAX_UPLOAD_FILE_BYTES → FileTooLarge
//!   - valid in-root regular file   → Ok(canonicalized)

use std::fs;
use std::path::{Path, PathBuf};

use crate::upload_guard::{validate_upload_paths, UploadError};

const MAX_FILES: usize = 20;
const MAX_BYTES: u64 = 100 * 1024 * 1024; // 100 MiB
const MAX_TOTAL: u64 = 200 * 1024 * 1024; // 200 MiB

fn tmp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("loom-upload-test-{tag}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    // canonicalize so prefix comparison is symmetric with the validator.
    fs::canonicalize(&dir).unwrap()
}

fn write_file(dir: &Path, name: &str, contents: &[u8]) -> PathBuf {
    let p = dir.join(name);
    fs::write(&p, contents).unwrap();
    p
}

#[test]
fn unset_root_fails_closed() {
    let root = tmp_root("unset");
    let f = write_file(&root, "a.txt", b"hi");
    let err = validate_upload_paths(
        &[f.to_string_lossy().into()],
        None,
        MAX_FILES,
        MAX_BYTES,
        MAX_TOTAL,
    )
    .unwrap_err();
    assert!(matches!(err, UploadError::RootNotConfigured));
    assert_eq!(err.kind(), "upload_root_not_configured");
}

#[test]
fn path_outside_root_is_blocked() {
    let root = tmp_root("outside-root");
    let other = tmp_root("outside-other");
    let f = write_file(&other, "secret.txt", b"nope");
    let err = validate_upload_paths(
        &[f.to_string_lossy().into()],
        Some(&root),
        MAX_FILES,
        MAX_BYTES,
        MAX_TOTAL,
    )
    .unwrap_err();
    assert!(matches!(err, UploadError::PathBlocked { .. }));
    assert_eq!(err.kind(), "upload_path_blocked");
}

#[test]
fn symlink_escape_is_blocked() {
    let root = tmp_root("symlink-root");
    let outside = tmp_root("symlink-outside");
    let target = write_file(&outside, "real.txt", b"escape");
    let link = root.join("link.txt");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let err = validate_upload_paths(
        &[link.to_string_lossy().into()],
        Some(&root),
        MAX_FILES,
        MAX_BYTES,
        MAX_TOTAL,
    )
    .unwrap_err();
    // canonicalize() resolves the symlink to `outside/real.txt`, not under root.
    assert!(matches!(err, UploadError::PathBlocked { .. }));
}

#[test]
fn nonexistent_path_not_found() {
    let root = tmp_root("missing");
    let ghost = root.join("does-not-exist.txt");
    let err = validate_upload_paths(
        &[ghost.to_string_lossy().into()],
        Some(&root),
        MAX_FILES,
        MAX_BYTES,
        MAX_TOTAL,
    )
    .unwrap_err();
    assert!(matches!(err, UploadError::PathNotFound { .. }));
    assert_eq!(err.kind(), "upload_path_not_found");
}

#[test]
fn too_many_files_rejected() {
    let root = tmp_root("too-many");
    let mut paths = Vec::new();
    for i in 0..(MAX_FILES + 1) {
        paths.push(
            write_file(&root, &format!("f{i}.txt"), b"x")
                .to_string_lossy()
                .into(),
        );
    }
    let err =
        validate_upload_paths(&paths, Some(&root), MAX_FILES, MAX_BYTES, MAX_TOTAL).unwrap_err();
    assert!(matches!(err, UploadError::TooManyFiles { .. }));
    assert_eq!(err.kind(), "upload_too_many_files");
}

#[test]
fn aggregate_size_over_total_cap_rejected() {
    // Each file is under the per-file cap, but together they exceed the total
    // cap. Use sparse files so no real bytes are written.
    let root = tmp_root("total");
    let per_file = MAX_BYTES; // 100 MiB each; 3 × 100 MiB = 300 MiB > 200 MiB total
    let mut paths = Vec::new();
    for i in 0..3 {
        let p = root.join(format!("big{i}.bin"));
        let f = fs::File::create(&p).unwrap();
        f.set_len(per_file).unwrap();
        paths.push(p.to_string_lossy().into());
    }
    let err =
        validate_upload_paths(&paths, Some(&root), MAX_FILES, MAX_BYTES, MAX_TOTAL).unwrap_err();
    assert!(matches!(err, UploadError::TotalTooLarge { .. }));
    assert_eq!(err.kind(), "upload_total_too_large");
}

#[test]
fn oversize_file_rejected() {
    let root = tmp_root("oversize");
    let big = root.join("big.bin");
    let f = fs::File::create(&big).unwrap();
    f.set_len(MAX_BYTES + 1).unwrap(); // sparse — no real bytes written
    let err = validate_upload_paths(
        &[big.to_string_lossy().into()],
        Some(&root),
        MAX_FILES,
        MAX_BYTES,
        MAX_TOTAL,
    )
    .unwrap_err();
    assert!(matches!(err, UploadError::FileTooLarge { .. }));
    assert_eq!(err.kind(), "upload_file_too_large");
}

#[test]
fn valid_in_root_file_ok() {
    let root = tmp_root("valid");
    let f = write_file(&root, "ok.txt", b"hello");
    let out = validate_upload_paths(
        &[f.to_string_lossy().into()],
        Some(&root),
        MAX_FILES,
        MAX_BYTES,
        MAX_TOTAL,
    )
    .unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0], fs::canonicalize(&f).unwrap());
}
