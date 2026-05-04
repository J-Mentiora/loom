// Re-export of the locked Phase 5.3 interface. DO NOT EDIT here.
// Edit `systems/loom-cli/modules/ChromiumDownloader/interfaces.rs` instead.
// ChromiumDownloader — pinned-URL Chromium download with sha256
// verification.
//
// # Contract semantics
// - **SR-CLI-04.** sha256 mismatch returns
//   `LoomErrorCode::SupplyChainViolation { expected_hash,
//   actual_hash, url }` (mapped to `CliError::SupplyChain` by
//   `ErrorMapper`).
// - **Idempotent (AC-CHPLUMB-02).** `ensure(url, expected_sha256)` skips
//   download if the binary is present AND `install_dir/.archive_sha256`
//   contains the expected SHA. The sentinel stores the archive SHA so the
//   idempotence check is semantically correct (archive SHA ≠ binary SHA).
// - **Extraction (AC-CHPLUMB-01).** After SHA verify the zip archive is
//   extracted via the `zip` crate into `install_dir`. The sentinel is
//   written only after successful extraction.
// - **Internal-only.** Used by `PostinstallRunner` (download path) +
//   `DoctorRunner` (verify-only path, AC-CHPLUMB-03).

use std::path::{Path, PathBuf};

use crate::CliError;

/// Outcome of a `ensure` call. `Skipped` means the binary was already
/// present + verified via sentinel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadOutcome {
    Skipped,
    Downloaded(PathBuf),
}

/// Configuration for the downloader.
#[derive(Debug, Clone)]
pub struct ChromiumDownloaderConfig {
    /// Install root (e.g. `~/.../chromium/`).
    pub install_dir: PathBuf,
    /// Final binary subpath relative to `install_dir`
    /// (e.g. `Chromium.app/Contents/MacOS/Chromium`).
    pub binary_subpath: PathBuf,
}

/// Stateless handle. Holds config only; curl is invoked as a subprocess.
pub struct ChromiumDownloader {
    pub(crate) config: ChromiumDownloaderConfig,
}

impl ChromiumDownloader {
    pub fn new(config: ChromiumDownloaderConfig) -> Self {
        Self { config }
    }

    /// Idempotent ensure-present (AC-CHPLUMB-02): return `Skipped` when
    /// the binary exists and `install_dir/.archive_sha256` contains the
    /// expected SHA. Otherwise: download archive → verify SHA → extract
    /// zip → write sentinel.
    pub async fn ensure(
        &self,
        url: &str,
        expected_sha256: &str,
    ) -> Result<DownloadOutcome, CliError> {
        let binary = self.binary_path();
        let sentinel = sentinel_path(&self.config.install_dir);

        // Idempotence check: binary present AND sentinel matches.
        if binary.exists() && sentinel.exists() {
            let recorded = std::fs::read_to_string(&sentinel)
                .map_err(|e| CliError::Internal(format!("read sentinel: {e}")))?;
            if recorded.trim() == expected_sha256 {
                return Ok(DownloadOutcome::Skipped);
            }
        }

        // Prepare install_dir and temp download path.
        std::fs::create_dir_all(&self.config.install_dir)
            .map_err(|e| CliError::Internal(format!("create dir: {e}")))?;
        let tmp = self.config.install_dir.join(".chromium.download.tmp");

        // Download the archive.
        let status = std::process::Command::new("curl")
            .args(["-fsSL", "-L", "-o"])
            .arg(&tmp)
            .arg(url)
            .status()
            .map_err(|e| CliError::Internal(format!("curl: {e}")))?;
        if !status.success() {
            return Err(CliError::Internal(format!("curl download failed for {url}")));
        }

        // Verify archive SHA-256 before extraction.
        let actual = sha256_of_file(&tmp).await?;
        if actual != expected_sha256 {
            let _ = std::fs::remove_file(&tmp);
            return Err(CliError::SupplyChain {
                expected_hash: expected_sha256.to_string(),
                actual_hash: actual,
                url: url.to_string(),
            });
        }

        // Extract zip archive into install_dir (AC-CHPLUMB-01).
        extract_zip(&tmp, &self.config.install_dir)?;
        let _ = std::fs::remove_file(&tmp);

        // Write sentinel with archive SHA (AC-CHPLUMB-02).
        std::fs::write(&sentinel, expected_sha256)
            .map_err(|e| CliError::Internal(format!("write sentinel: {e}")))?;

        Ok(DownloadOutcome::Downloaded(binary))
    }

    /// Verify-only path used by `DoctorRunner` check 4 (AC-CHPLUMB-03).
    /// Does NOT download; returns `Ok(())` when the binary exists and
    /// the sentinel matches `expected_sha256`.
    ///
    /// Fallback (AC-CHBS-01): when the literal `binary_path()` is missing but
    /// its parent directory (e.g. `Contents/MacOS/`) exists and contains at
    /// least one executable file, returns `Ok(())` without a sentinel check.
    /// This covers whole-bundle symlinks to differently-named browsers such as
    /// `Google Chrome.app` (AC-CHBS-02).
    pub async fn verify(&self, expected_sha256: &str) -> Result<(), CliError> {
        let binary = self.binary_path();
        if !binary.exists() {
            // Fallback: scan the parent directory for any executable.
            // Covers bundles where the browser binary name differs from the
            // pinned subpath (e.g. "Google Chrome" instead of "Chromium").
            // System installations have no .archive_sha256 sentinel, so we
            // skip the sentinel check on this path.
            if let Some(parent) = binary.parent() {
                if parent.is_dir() && find_executable_in_dir(parent)?.is_some() {
                    return Ok(());
                }
            }
            return Err(CliError::Internal(format!(
                "chromium binary not found: {} — run loom postinstall",
                binary.display()
            )));
        }
        let sentinel = sentinel_path(&self.config.install_dir);
        if !sentinel.exists() {
            return Err(CliError::Internal(format!(
                "chromium sentinel missing: {} — run loom postinstall",
                sentinel.display()
            )));
        }
        let recorded = std::fs::read_to_string(&sentinel)
            .map_err(|e| CliError::Internal(format!("read sentinel: {e}")))?;
        if recorded.trim() != expected_sha256 {
            return Err(CliError::SupplyChain {
                expected_hash: expected_sha256.to_string(),
                actual_hash: recorded.trim().to_string(),
                url: sentinel.to_string_lossy().to_string(),
            });
        }
        Ok(())
    }

    /// Returns the absolute path to the final Chromium binary. Used
    /// by `DoctorRunner` for the existence probe.
    pub fn binary_path(&self) -> PathBuf {
        self.config.install_dir.join(&self.config.binary_subpath)
    }
}

/// Returns the sentinel file path for the given install directory.
/// The sentinel stores the archive SHA-256 for idempotence checking.
fn sentinel_path(install_dir: &Path) -> PathBuf {
    install_dir.join(".archive_sha256")
}

/// Scan `dir` for the first executable regular file (follows symlinks).
/// Returns `Some(path)` when found, `None` when the directory contains no
/// executable files. Returns `Err` only when the directory itself cannot be
/// read.
///
/// On Unix, a file is considered executable when any of the owner/group/other
/// execute bits are set (`mode & 0o111 != 0`). On non-Unix platforms every
/// regular file is considered executable.
fn find_executable_in_dir(dir: &Path) -> Result<Option<PathBuf>, CliError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| CliError::Internal(format!("read dir {}: {e}", dir.display())))?;
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        // Follow symlinks so a dangling symlink is safely skipped.
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            if meta.mode() & 0o111 != 0 {
                return Ok(Some(path));
            }
        }
        #[cfg(not(unix))]
        {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

/// Extract a zip archive into `dest_dir`. Each entry is written relative
/// to `dest_dir`. Unix executable permissions are restored when available.
fn extract_zip(archive: &Path, dest_dir: &Path) -> Result<(), CliError> {
    let file = std::fs::File::open(archive)
        .map_err(|e| CliError::Internal(format!("open archive: {e}")))?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| CliError::Internal(format!("parse zip: {e}")))?;

    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| CliError::Internal(format!("zip entry {i}: {e}")))?;
        let entry_path = dest_dir.join(entry.name());

        if entry.is_dir() {
            std::fs::create_dir_all(&entry_path).map_err(|e| {
                CliError::Internal(format!("create dir {}: {e}", entry_path.display()))
            })?;
        } else {
            if let Some(parent) = entry_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    CliError::Internal(format!("create dir {}: {e}", parent.display()))
                })?;
            }
            let mut out = std::fs::File::create(&entry_path)
                .map_err(|e| CliError::Internal(format!("create {}: {e}", entry_path.display())))?;
            std::io::copy(&mut entry, &mut out)
                .map_err(|e| CliError::Internal(format!("extract {}: {e}", entry_path.display())))?;

            #[cfg(unix)]
            if let Some(mode) = entry.unix_mode() {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&entry_path, std::fs::Permissions::from_mode(mode))
                    .map_err(|e| {
                        CliError::Internal(format!("chmod {}: {e}", entry_path.display()))
                    })?;
            }
        }
    }
    Ok(())
}

/// Pure helper: compute sha256 of a file path. Delegates to `shasum`
/// (macOS) or `sha256sum` (Linux). Returns lowercase hex.
pub async fn sha256_of_file(path: &Path) -> Result<String, CliError> {
    let output = std::process::Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .or_else(|_| std::process::Command::new("sha256sum").arg(path).output())
        .map_err(|e| CliError::Internal(format!("sha256 command: {e}")))?;
    if !output.status.success() {
        return Err(CliError::Internal(format!(
            "sha256 command failed for {}",
            path.display()
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.split_whitespace().next().unwrap_or("").to_string())
}
