// Integration tests for chromium-sha-pin-real ACs.
//
// AC-CHRPIN-03: `loom postinstall` downloads, verifies SHA, extracts — binary executable.
// AC-CHRPIN-04: Corrupted archive → CliError::SupplyChain, not exit 0.
// AC-CHRPIN-05: Tests use a local HTTP server; both happy and tampered paths covered.
//
// Run with: cargo test -p loom-cli --test integration_chromium_postinstall

use loom_cli::chromium_downloader::{ChromiumDownloader, ChromiumDownloaderConfig};
use loom_cli::error_mapper::CliError;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

// ── helpers ──────────────────────────────────────────────────────────────────

/// Create a minimal zip archive containing one entry `entry_name` with
/// the given bytes as content. Returns the raw zip bytes.
fn make_test_zip(entry_name: &str, content: &[u8]) -> Vec<u8> {
    use std::io::Write as _;
    use zip::write::{SimpleFileOptions, ZipWriter};

    let buf = std::io::Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(buf);
    let options = SimpleFileOptions::default().unix_permissions(0o755);
    zip.start_file(entry_name, options).unwrap();
    zip.write_all(content).unwrap();
    let cursor = zip.finish().unwrap();
    cursor.into_inner()
}

/// Compute SHA-256 of raw bytes and return lowercase hex string.
fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// Spawn a one-shot HTTP/1.1 server on an OS-assigned port that serves
/// `zip_bytes` as the response body. Returns `(url, join_handle)`.
/// The server closes after the first accepted connection.
fn spawn_one_shot_server(zip_bytes: Vec<u8>) -> (String, std::thread::JoinHandle<()>) {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local server");
    let port = listener.local_addr().unwrap().port();

    let handle = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            // Drain request headers so curl doesn't get a connection reset.
            let mut req_buf = [0u8; 4096];
            let _ = stream.read(&mut req_buf);

            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                zip_bytes.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&zip_bytes);
        }
    });

    let url = format!("http://127.0.0.1:{port}/chromium.zip");
    (url, handle)
}

// ── AC-CHRPIN-03: happy path download + extract + binary executable ───────────

/// AC-CHRPIN-03 / AC-CHRPIN-05 (happy path):
/// ChromiumDownloader::ensure() with a correct SHA-256 must download the
/// archive, verify SHA, extract the binary, and return Downloaded.
#[tokio::test]
async fn test_ac_chrpin_03_happy_path_download_and_extract() {
    let install_dir = TempDir::new().unwrap();

    // Build a zip containing our "binary" at the subpath.
    let binary_subpath = "chromium";
    let zip_bytes = make_test_zip(binary_subpath, b"#!/bin/sh\necho 'fake chromium'\n");
    let expected_sha = sha256_hex(&zip_bytes);

    let (url, server) = spawn_one_shot_server(zip_bytes);

    let downloader = ChromiumDownloader::new(ChromiumDownloaderConfig {
        install_dir: install_dir.path().to_path_buf(),
        binary_subpath: binary_subpath.into(),
    });

    let outcome = downloader
        .ensure(&url, &expected_sha)
        .await
        .expect("AC-CHRPIN-03: ensure() must not return Err on valid archive");

    // AC-CHRPIN-03: outcome must be Downloaded, not Skipped.
    assert!(
        matches!(
            outcome,
            loom_cli::chromium_downloader::DownloadOutcome::Downloaded(_)
        ),
        "AC-CHRPIN-03: expected Downloaded, got {:?}",
        outcome
    );

    // Binary must exist after extraction.
    let binary_path = install_dir.path().join(binary_subpath);
    assert!(
        binary_path.exists(),
        "AC-CHRPIN-03: binary must exist at {binary_path:?} after ensure()"
    );

    // Binary must be executable on Unix (AC-CHRPIN-03 requirement).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&binary_path).unwrap().permissions().mode();
        assert!(
            mode & 0o111 != 0,
            "AC-CHRPIN-03: binary must be executable, mode={mode:o}"
        );
    }

    let _ = server.join();
}

// ── AC-CHRPIN-04: supply-chain mismatch → CliError::SupplyChain ───────────────

/// AC-CHRPIN-04 / AC-CHRPIN-05 (tampered archive path):
/// ChromiumDownloader::ensure() with a wrong SHA-256 must return
/// CliError::SupplyChain (not exit 0, not CliError::Internal).
#[tokio::test]
async fn test_ac_chrpin_04_supply_chain_tampered_archive() {
    let install_dir = TempDir::new().unwrap();

    let binary_subpath = "chromium";
    let zip_bytes = make_test_zip(binary_subpath, b"#!/bin/sh\necho 'tampered chromium'\n");

    // Deliberately provide wrong SHA-256 (simulate corrupted/tampered archive).
    let wrong_sha = "0".repeat(64);

    let (url, server) = spawn_one_shot_server(zip_bytes);

    let downloader = ChromiumDownloader::new(ChromiumDownloaderConfig {
        install_dir: install_dir.path().to_path_buf(),
        binary_subpath: binary_subpath.into(),
    });

    let result = downloader.ensure(&url, &wrong_sha).await;

    // AC-CHRPIN-04: must be Err, and specifically CliError::SupplyChain.
    assert!(result.is_err(), "AC-CHRPIN-04: ensure() must return Err on SHA mismatch");

    match result.unwrap_err() {
        CliError::SupplyChain { expected_hash, actual_hash, .. } => {
            assert_eq!(
                expected_hash, wrong_sha,
                "AC-CHRPIN-04: SupplyChain::expected_hash must be the SHA we passed"
            );
            assert_ne!(
                actual_hash, wrong_sha,
                "AC-CHRPIN-04: SupplyChain::actual_hash must differ from the wrong SHA"
            );
        }
        other => panic!(
            "AC-CHRPIN-04: expected CliError::SupplyChain, got: {other:?}"
        ),
    }

    // Binary must NOT exist after a failed ensure (archive not extracted).
    let binary_path = install_dir.path().join(binary_subpath);
    assert!(
        !binary_path.exists(),
        "AC-CHRPIN-04: binary must not exist after supply-chain failure"
    );

    let _ = server.join();
}

// ── AC-CHRPIN-05: idempotence — second ensure() with matching sentinel → Skipped

/// Bonus: second call to ensure() when sentinel already present → Skipped.
/// Not in the AC set but validates idempotence invariant from chromium-plumbing-fix.
#[tokio::test]
async fn test_idempotent_second_ensure_returns_skipped() {
    let install_dir = TempDir::new().unwrap();

    let binary_subpath = "chromium";
    let zip_bytes = make_test_zip(binary_subpath, b"#!/bin/sh\necho 'chromium'\n");
    let expected_sha = sha256_hex(&zip_bytes);

    let (url, server) = spawn_one_shot_server(zip_bytes.clone());

    let downloader = ChromiumDownloader::new(ChromiumDownloaderConfig {
        install_dir: install_dir.path().to_path_buf(),
        binary_subpath: binary_subpath.into(),
    });

    // First call: downloads.
    downloader.ensure(&url, &expected_sha).await.expect("first ensure must succeed");

    // Second call: sentinel present, should return Skipped without network.
    // We reuse the same downloader; no server running (port is closed).
    let outcome2 = downloader
        .ensure("http://127.0.0.1:1/unreachable", &expected_sha)
        .await
        .expect("second ensure must not fail when sentinel is present");

    assert!(
        matches!(
            outcome2,
            loom_cli::chromium_downloader::DownloadOutcome::Skipped
        ),
        "Expected Skipped on second call, got {:?}",
        outcome2
    );

    let _ = server.join();
}
