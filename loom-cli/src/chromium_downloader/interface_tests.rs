// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-cli/modules/ChromiumDownloader/interface_tests.rs` instead.
// Interface tests for `ChromiumDownloader`. Verifies SR-CLI-04
// supply-chain mismatch shape, the idempotent-ensure contract (sentinel
// file model — AC-CHPLUMB-02), extraction (AC-CHPLUMB-01), and the
// `DoctorRunner`-shared verify-only path (AC-CHPLUMB-03).

use super::chromium_downloader::{
    sha256_of_file, ChromiumDownloader, ChromiumDownloaderConfig, DownloadOutcome,
};
use crate::CliError;

// ── Structural / compile-time tests ─────────────────────────────────────────

#[test]
fn config_carries_install_dir_and_binary_subpath() {
    let c = ChromiumDownloaderConfig {
        install_dir: "/tmp/chromium".into(),
        binary_subpath: "rev1/Chromium".into(),
    };
    assert!(c.install_dir.is_absolute());
}

#[test]
fn binary_path_joins_install_dir_and_subpath() {
    let d = ChromiumDownloader::new(ChromiumDownloaderConfig {
        install_dir: "/tmp/chromium".into(),
        binary_subpath: "rev1/Chromium".into(),
    });
    let p = d.binary_path();
    assert_eq!(p, std::path::PathBuf::from("/tmp/chromium/rev1/Chromium"));
}

#[test]
fn download_outcome_variant_set_locked() {
    fn _ck(o: DownloadOutcome) -> &'static str {
        match o {
            DownloadOutcome::Skipped => "skipped",
            DownloadOutcome::Downloaded(_) => "downloaded",
        }
    }
    let _ = _ck;
}

#[test]
fn ensure_signature() {
    fn _ck(d: &ChromiumDownloader, url: &str, sha: &str) {
        let _f = async move {
            let _: Result<DownloadOutcome, CliError> = d.ensure(url, sha).await;
        };
    }
    let _ = _ck;
}

#[test]
fn verify_signature() {
    fn _ck(d: &ChromiumDownloader, sha: &str) {
        let _f = async move {
            let _: Result<(), CliError> = d.verify(sha).await;
        };
    }
    let _ = _ck;
}

#[test]
fn sha256_of_file_signature() {
    fn _ck(p: &std::path::Path) {
        let _f = async move {
            let _: Result<String, CliError> = sha256_of_file(p).await;
        };
    }
    let _ = _ck;
}

// ── AC-CHPLUMB-02: ensure() idempotent via sentinel file ────────────────────

/// When install_dir/.archive_sha256 contains the expected SHA-256 AND the
/// binary exists, ensure() must return Skipped without downloading.
#[tokio::test]
async fn ensure_skips_when_sentinel_matches() {
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();

    let expected_sha = "aabbccdd00112233aabbccdd00112233aabbccdd00112233aabbccdd00112233";

    // Binary placeholder must exist.
    let binary_path = dir.path().join("Chromium");
    std::fs::write(&binary_path, b"fake-chromium-binary").unwrap();

    // Sentinel with matching SHA.
    let sentinel = dir.path().join(".archive_sha256");
    std::fs::write(&sentinel, expected_sha).unwrap();

    let downloader = ChromiumDownloader::new(ChromiumDownloaderConfig {
        install_dir: dir.path().to_path_buf(),
        binary_subpath: "Chromium".into(),
    });

    let result = downloader
        .ensure(
            "https://should-not-be-reached.invalid/chromium.zip",
            expected_sha,
        )
        .await;

    assert!(
        matches!(result, Ok(DownloadOutcome::Skipped)),
        "expected Skipped (sentinel matches), got: {result:?}"
    );
}

// ── AC-CHPLUMB-01: ensure() extracts archive after SHA-256 verify ───────────

/// Build a minimal valid zip served via file:// URL. After ensure(), the
/// extracted file must be present and the sentinel must contain the archive SHA.
#[tokio::test]
async fn ensure_extracts_zip_and_writes_sentinel() {
    use tempfile::TempDir;

    // Build a minimal zip containing a file at "Chromium" inside the archive.
    let zip_dir = TempDir::new().unwrap();
    let zip_path = zip_dir.path().join("chromium.zip");
    {
        use std::io::Write as _;
        let zip_file = std::fs::File::create(&zip_path).unwrap();
        let mut zw = zip::ZipWriter::new(zip_file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .unix_permissions(0o755);
        zw.start_file("Chromium", opts).unwrap();
        zw.write_all(b"fake-chromium-binary-content").unwrap();
        zw.finish().unwrap();
    }

    // Compute the real SHA-256 of the zip archive.
    let archive_sha = sha256_of_file(&zip_path).await.unwrap();

    let install_dir = TempDir::new().unwrap();
    let downloader = ChromiumDownloader::new(ChromiumDownloaderConfig {
        install_dir: install_dir.path().to_path_buf(),
        binary_subpath: "Chromium".into(),
    });

    let file_url = format!("file://{}", zip_path.display());
    let result = downloader.ensure(&file_url, &archive_sha).await;

    assert!(
        matches!(result, Ok(DownloadOutcome::Downloaded(_))),
        "expected Downloaded, got: {result:?}"
    );

    // AC-CHPLUMB-01: binary extracted (real file, not zip archive).
    let binary = install_dir.path().join("Chromium");
    assert!(binary.exists(), "binary must exist after extraction");
    let content = std::fs::read(&binary).unwrap();
    assert_eq!(
        content, b"fake-chromium-binary-content",
        "extracted content must match"
    );

    // AC-CHPLUMB-02: sentinel written after extraction.
    let sentinel = install_dir.path().join(".archive_sha256");
    assert!(
        sentinel.exists(),
        "sentinel .archive_sha256 must be written"
    );
    let recorded = std::fs::read_to_string(&sentinel).unwrap();
    assert_eq!(
        recorded.trim(),
        archive_sha,
        "sentinel must contain archive SHA-256"
    );
}

/// SHA mismatch → SupplyChain error; binary must NOT be extracted.
#[tokio::test]
async fn ensure_supply_chain_on_sha_mismatch() {
    use tempfile::TempDir;

    let zip_dir = TempDir::new().unwrap();
    let zip_path = zip_dir.path().join("chromium.zip");
    {
        use std::io::Write as _;
        let zip_file = std::fs::File::create(&zip_path).unwrap();
        let mut zw = zip::ZipWriter::new(zip_file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zw.start_file("Chromium", opts).unwrap();
        zw.write_all(b"real-content").unwrap();
        zw.finish().unwrap();
    }

    let install_dir = TempDir::new().unwrap();
    let downloader = ChromiumDownloader::new(ChromiumDownloaderConfig {
        install_dir: install_dir.path().to_path_buf(),
        binary_subpath: "Chromium".into(),
    });

    let file_url = format!("file://{}", zip_path.display());
    let wrong_sha = "0000000000000000000000000000000000000000000000000000000000000000";
    let result = downloader.ensure(&file_url, wrong_sha).await;

    assert!(
        matches!(result, Err(CliError::SupplyChain { .. })),
        "expected SupplyChain error, got: {result:?}"
    );

    // Binary must NOT be present after SHA mismatch.
    let binary = install_dir.path().join("Chromium");
    assert!(!binary.exists(), "binary must not exist after SHA mismatch");
}

// ── AC-CHPLUMB-03: verify() checks binary + sentinel ────────────────────────

/// Binary + matching sentinel → Ok(()).
#[tokio::test]
async fn verify_ok_when_sentinel_matches() {
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("Chromium"), b"fake-binary").unwrap();

    let expected_sha = "cafebabe00000000cafebabe00000000cafebabe00000000cafebabe00000000";
    std::fs::write(dir.path().join(".archive_sha256"), expected_sha).unwrap();

    let downloader = ChromiumDownloader::new(ChromiumDownloaderConfig {
        install_dir: dir.path().to_path_buf(),
        binary_subpath: "Chromium".into(),
    });

    let result = downloader.verify(expected_sha).await;
    assert!(
        result.is_ok(),
        "verify must return Ok when sentinel matches: {result:?}"
    );
}

/// Binary + wrong sentinel → SupplyChainViolation.
#[tokio::test]
async fn verify_supply_chain_on_sentinel_mismatch() {
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("Chromium"), b"fake-binary").unwrap();
    std::fs::write(
        dir.path().join(".archive_sha256"),
        "aaaa0000aaaa0000aaaa0000aaaa0000aaaa0000aaaa0000aaaa0000aaaa0000",
    )
    .unwrap();

    let downloader = ChromiumDownloader::new(ChromiumDownloaderConfig {
        install_dir: dir.path().to_path_buf(),
        binary_subpath: "Chromium".into(),
    });

    let result = downloader
        .verify("bbbb1111bbbb1111bbbb1111bbbb1111bbbb1111bbbb1111bbbb1111bbbb1111")
        .await;

    assert!(
        matches!(result, Err(CliError::SupplyChain { .. })),
        "expected SupplyChain on sentinel mismatch, got: {result:?}"
    );
}

/// Binary exists but no sentinel → Internal (incomplete install).
#[tokio::test]
async fn verify_internal_on_missing_sentinel() {
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("Chromium"), b"fake-binary").unwrap();
    // No sentinel.

    let downloader = ChromiumDownloader::new(ChromiumDownloaderConfig {
        install_dir: dir.path().to_path_buf(),
        binary_subpath: "Chromium".into(),
    });

    let result = downloader
        .verify("cafebabe00000000cafebabe00000000cafebabe00000000cafebabe00000000")
        .await;

    assert!(
        matches!(result, Err(CliError::Internal(_))),
        "expected Internal on missing sentinel, got: {result:?}"
    );
}

// ── AC-CHBS-01: verify() falls back to scanning parent dir for executables ───

/// When the literal binary_path is missing but a differently-named executable
/// exists in the same Contents/MacOS/ directory, verify() must return Ok(()).
#[tokio::test]
async fn verify_fallback_scan_finds_differently_named_binary() {
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();

    // Create Contents/MacOS/ directory with a differently-named executable.
    let macos_dir = dir.path().join("Contents").join("MacOS");
    std::fs::create_dir_all(&macos_dir).unwrap();
    let alt_binary = macos_dir.join("Google Chrome");
    std::fs::write(&alt_binary, b"fake-chrome-binary").unwrap();

    // Make it executable on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&alt_binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // ChromiumDownloader configured for the non-existent 'Chromium' binary.
    let downloader = ChromiumDownloader::new(ChromiumDownloaderConfig {
        install_dir: dir.path().to_path_buf(),
        binary_subpath: "Contents/MacOS/Chromium".into(),
    });

    // binary_path() = Contents/MacOS/Chromium — does not exist.
    // Fallback should scan Contents/MacOS/ and find 'Google Chrome'.
    let result = downloader.verify("any-sha-unused").await;
    assert!(
        result.is_ok(),
        "AC-CHBS-01: fallback scan must return Ok when executable found in parent dir; got: {result:?}"
    );
}

// ── AC-CHBS-03: verify() fails when no executable exists in parent ───────────

/// When the literal binary_path is missing AND the parent directory contains
/// no executable files, verify() must still return Err(Internal).
#[tokio::test]
async fn verify_fails_when_no_executable_in_parent() {
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();

    // Create Contents/MacOS/ directory with only a non-executable file.
    let macos_dir = dir.path().join("Contents").join("MacOS");
    std::fs::create_dir_all(&macos_dir).unwrap();
    let non_exec = macos_dir.join("README.txt");
    std::fs::write(&non_exec, b"not a binary").unwrap();
    // Do NOT set executable permission — mode defaults to 0o644.

    let downloader = ChromiumDownloader::new(ChromiumDownloaderConfig {
        install_dir: dir.path().to_path_buf(),
        binary_subpath: "Contents/MacOS/Chromium".into(),
    });

    let result = downloader.verify("any-sha-unused").await;
    assert!(
        matches!(result, Err(CliError::Internal(_))),
        "AC-CHBS-03: must return Internal when no executable in parent dir; got: {result:?}"
    );
}
