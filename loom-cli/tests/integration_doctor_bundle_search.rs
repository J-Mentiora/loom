//! Integration tests for `chromium-bundle-search` feature.
//! Covers AC-CHBS-02 and AC-CHBS-04.
//!
//! AC-CHBS-04: build a fake bundle with a binary at a non-default name
//! and assert verify() succeeds.
//! AC-CHBS-02: whole-bundle symlink scenario (simulated via rename).

use loom_cli::chromium_downloader::{ChromiumDownloader, ChromiumDownloaderConfig};
use loom_cli::CliError;
use tempfile::TempDir;

// ── AC-CHBS-04: fake bundle with non-default binary name ────────────────────

/// Build a fake Chromium.app bundle where the binary inside Contents/MacOS/
/// is named 'Google Chrome' (not 'Chromium'). ChromiumDownloader::verify()
/// must return Ok(()) via the fallback scan.
#[tokio::test]
async fn test_verify_ok_with_differently_named_binary_in_bundle() {
    let dir = TempDir::new().unwrap();

    // Build: install_dir/Chromium.app/Contents/MacOS/Google Chrome
    let macos_dir = dir
        .path()
        .join("Chromium.app")
        .join("Contents")
        .join("MacOS");
    std::fs::create_dir_all(&macos_dir).unwrap();

    let browser_binary = macos_dir.join("Google Chrome");
    std::fs::write(&browser_binary, b"fake-google-chrome-binary").unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            &browser_binary,
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }

    // Downloader expects the standard subpath — literal binary doesn't exist.
    let downloader = ChromiumDownloader::new(ChromiumDownloaderConfig {
        install_dir: dir.path().to_path_buf(),
        binary_subpath: "Chromium.app/Contents/MacOS/Chromium".into(),
    });

    // AC-CHBS-04: verify() must find 'Google Chrome' via fallback scan.
    let result = downloader.verify("any-sha-unused").await;
    assert!(
        result.is_ok(),
        "AC-CHBS-04: verify() must return Ok when non-default binary found in Contents/MacOS/; got: {result:?}"
    );
}

// ── AC-CHBS-02: symlinked bundle scenario ────────────────────────────────────

/// Simulate a whole-bundle symlink: install_dir/Chromium.app is a symlink
/// to a 'Google Chrome.app' directory. The binary inside is 'Google Chrome'.
/// verify() must return Ok(()).
#[tokio::test]
#[cfg(unix)]
async fn test_verify_ok_with_symlinked_bundle() {
    let dir = TempDir::new().unwrap();

    // Create the "real" Google Chrome.app in a separate location.
    let chrome_app = dir.path().join("Google Chrome.app");
    let chrome_macos = chrome_app.join("Contents").join("MacOS");
    std::fs::create_dir_all(&chrome_macos).unwrap();
    let chrome_binary = chrome_macos.join("Google Chrome");
    std::fs::write(&chrome_binary, b"fake-google-chrome-binary").unwrap();

    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(
        &chrome_binary,
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    // Create install_dir and symlink Chromium.app → Google Chrome.app.
    let install_dir = dir.path().join("install");
    std::fs::create_dir_all(&install_dir).unwrap();
    std::os::unix::fs::symlink(&chrome_app, install_dir.join("Chromium.app")).unwrap();

    // Downloader configured for the standard subpath.
    let downloader = ChromiumDownloader::new(ChromiumDownloaderConfig {
        install_dir: install_dir.clone(),
        binary_subpath: "Chromium.app/Contents/MacOS/Chromium".into(),
    });

    // binary_path() = install/Chromium.app/Contents/MacOS/Chromium
    // → resolves through symlink to Google Chrome.app/Contents/MacOS/Chromium
    // → does not exist; fallback scans Contents/MacOS/ and finds 'Google Chrome'.
    let result = downloader.verify("any-sha-unused").await;
    assert!(
        result.is_ok(),
        "AC-CHBS-02: verify() must return Ok against a whole-bundle symlink to Google Chrome.app; got: {result:?}"
    );
}

// ── AC-CHBS-03: no executable anywhere → fail ────────────────────────────────

/// When the literal binary_path is missing and Contents/MacOS/ contains
/// no executables, verify() must return Err(Internal) — not Ok.
#[tokio::test]
async fn test_verify_fails_when_no_executable_in_bundle() {
    let dir = TempDir::new().unwrap();

    // Build a bundle with only a non-executable README inside Contents/MacOS/.
    let macos_dir = dir
        .path()
        .join("Chromium.app")
        .join("Contents")
        .join("MacOS");
    std::fs::create_dir_all(&macos_dir).unwrap();
    std::fs::write(macos_dir.join("README"), b"not a binary").unwrap();
    // No executable permissions set.

    let downloader = ChromiumDownloader::new(ChromiumDownloaderConfig {
        install_dir: dir.path().to_path_buf(),
        binary_subpath: "Chromium.app/Contents/MacOS/Chromium".into(),
    });

    let result = downloader.verify("any-sha-unused").await;
    assert!(
        matches!(result, Err(CliError::Internal(_))),
        "AC-CHBS-03: must return Internal when no executable found; got: {result:?}"
    );
}
