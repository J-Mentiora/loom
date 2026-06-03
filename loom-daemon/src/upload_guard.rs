//! Upload allow-list gate for `web.set_input_files`.
//!
//! This is the **authoritative security gate** for the file-upload verb. It runs
//! daemon-side in `dispatch_action_blocking` BEFORE the action reaches the wasm
//! guest or chromium (plan-council FND#5: most-trusted, earliest component — the
//! same layer as the evaluate-denylist gate). The wasm guest is sandboxed (no
//! filesystem) and cannot perform real path checks.
//!
//! Policy (decisions.md):
//!   - D-SEC-1: `LOOM_UPLOAD_ROOT` unset → fail closed (`RootNotConfigured`).
//!   - D-SEC-2: enforced in ALL profiles (the daemon calls this unconditionally,
//!     not gated on `session.profile == "safe"`).
//!   - D-CAPS-1: `MAX_UPLOAD_FILES` / `MAX_UPLOAD_FILE_BYTES` caps.
//!
//! Symlink-escape defense: every path AND the root are `std::fs::canonicalize`d,
//! then the canonical path must be prefixed by the canonical root. A symlink
//! inside the root pointing outside resolves (via canonicalize) to an
//! outside-root path and is rejected as `PathBlocked`. Returns the canonicalized
//! paths so the caller forwards those to chromium (closing most of the TOCTOU
//! window between validation and the browser's read).

use std::path::{Path, PathBuf};

/// Max number of files accepted in a single `web.set_input_files` call.
pub const MAX_UPLOAD_FILES: usize = 20;
/// Max size (bytes) of any single uploaded file. 100 MiB.
pub const MAX_UPLOAD_FILE_BYTES: u64 = 100 * 1024 * 1024;
/// Max aggregate size (bytes) across all files in one call. 200 MiB.
/// Bounds total work even when each file is individually under the per-file cap
/// (20 × 100 MiB would otherwise be 2 GiB).
pub const MAX_UPLOAD_TOTAL_BYTES: u64 = 200 * 1024 * 1024;

/// Typed rejection reasons. `kind()` is the stable wire-`kind` string surfaced
/// on the receipt error (no full host paths in the kind — see L1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadError {
    /// `LOOM_UPLOAD_ROOT` is not configured — fail closed.
    RootNotConfigured,
    /// Path (after canonicalization) is not under the allow-list root, or a
    /// symlink escaped the root.
    PathBlocked { path: String },
    /// Path does not exist or is not a regular file.
    PathNotFound { path: String },
    /// More than `MAX_UPLOAD_FILES` paths in one call.
    TooManyFiles { count: usize, max: usize },
    /// A file exceeds `MAX_UPLOAD_FILE_BYTES`.
    FileTooLarge { path: String, size: u64, max: u64 },
    /// The aggregate size of all files exceeds `MAX_UPLOAD_TOTAL_BYTES`.
    TotalTooLarge { total: u64, max: u64 },
}

impl UploadError {
    /// Stable wire `kind` string (hoisted onto the receipt error by the daemon's
    /// `build_wire_receipt_error`). Consumed by the Studio + e2e assertions.
    pub fn kind(&self) -> &'static str {
        match self {
            UploadError::RootNotConfigured => "upload_root_not_configured",
            UploadError::PathBlocked { .. } => "upload_path_blocked",
            UploadError::PathNotFound { .. } => "upload_path_not_found",
            UploadError::TooManyFiles { .. } => "upload_too_many_files",
            UploadError::FileTooLarge { .. } => "upload_file_too_large",
            UploadError::TotalTooLarge { .. } => "upload_total_too_large",
        }
    }

    /// Operator-facing message. Uses the file basename, not the full host path,
    /// to avoid leaking absolute paths into logs/receipts (L1).
    pub fn message(&self) -> String {
        match self {
            UploadError::RootNotConfigured => {
                "file upload denied: LOOM_UPLOAD_ROOT is not configured".to_string()
            }
            UploadError::PathBlocked { path } => {
                format!(
                    "file upload denied: {} is outside the allowed upload root",
                    basename(path)
                )
            }
            UploadError::PathNotFound { path } => {
                format!(
                    "file upload denied: {} does not exist or is not a regular file",
                    basename(path)
                )
            }
            UploadError::TooManyFiles { count, max } => {
                format!("file upload denied: {count} files exceeds the per-call limit of {max}")
            }
            UploadError::FileTooLarge { path, size, max } => {
                format!("file upload denied: {} is {size} bytes, exceeding the {max}-byte per-file limit", basename(path))
            }
            UploadError::TotalTooLarge { total, max } => {
                format!(
                    "file upload denied: {total} total bytes exceeds the {max}-byte per-call limit"
                )
            }
        }
    }
}

fn basename(p: &str) -> String {
    Path::new(p)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "<file>".to_string())
}

/// Validate `paths` against the upload allow-list and caps.
///
/// On success returns the canonicalized paths (forward THESE to chromium).
/// `upload_root` is `None` when `LOOM_UPLOAD_ROOT` is unset → fail closed.
pub fn validate_upload_paths(
    paths: &[String],
    upload_root: Option<&Path>,
    max_files: usize,
    max_bytes: u64,
    max_total_bytes: u64,
) -> Result<Vec<PathBuf>, UploadError> {
    // Fail closed before touching the filesystem.
    let root = upload_root.ok_or(UploadError::RootNotConfigured)?;

    // Count cap first (cheap; bounds the per-file work below).
    if paths.len() > max_files {
        return Err(UploadError::TooManyFiles {
            count: paths.len(),
            max: max_files,
        });
    }

    // Canonicalize the root once so the prefix comparison is symmetric (handles
    // symlinked/relative roots and case-normalizing filesystems).
    let canon_root = std::fs::canonicalize(root).map_err(|_| UploadError::RootNotConfigured)?;

    let mut out = Vec::with_capacity(paths.len());
    let mut total: u64 = 0;
    for raw in paths {
        // canonicalize resolves symlinks and requires the path to exist.
        let canon = std::fs::canonicalize(raw)
            .map_err(|_| UploadError::PathNotFound { path: raw.clone() })?;

        let meta = std::fs::metadata(&canon)
            .map_err(|_| UploadError::PathNotFound { path: raw.clone() })?;
        if !meta.is_file() {
            return Err(UploadError::PathNotFound { path: raw.clone() });
        }

        // Under-root containment (symlink-escape becomes outside-root here).
        if !canon.starts_with(&canon_root) {
            return Err(UploadError::PathBlocked { path: raw.clone() });
        }

        if meta.len() > max_bytes {
            return Err(UploadError::FileTooLarge {
                path: raw.clone(),
                size: meta.len(),
                max: max_bytes,
            });
        }

        total = total.saturating_add(meta.len());
        if total > max_total_bytes {
            return Err(UploadError::TotalTooLarge {
                total,
                max: max_total_bytes,
            });
        }

        out.push(canon);
    }
    Ok(out)
}
