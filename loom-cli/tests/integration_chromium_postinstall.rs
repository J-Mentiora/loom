// Integration tests for chromium-sha-pin-real.
//
// `loom postinstall` downloads, verifies SHA, extracts — binary executable.
// Corrupted archive → CliError::SupplyChain, not exit 0.
// Tests use a local HTTP server; both happy and tampered paths covered.
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

/// Like [`make_test_zip`] but stores the entry uncompressed so the archive's
/// on-wire byte length is ≥ `content.len()`. The progress test needs the
/// download to span multiple `PROGRESS_CHUNK_SIZE` (64 KiB) chunks; the
/// default deflate path would shrink a repetitive payload to a few hundred
/// bytes (one chunk → one callback).
fn make_stored_zip(entry_name: &str, content: &[u8]) -> Vec<u8> {
    use std::io::Write as _;
    use zip::write::{SimpleFileOptions, ZipWriter};

    let buf = std::io::Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(buf);
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .unix_permissions(0o755);
    zip.start_file(entry_name, options).unwrap();
    zip.write_all(content).unwrap();
    zip.finish().unwrap().into_inner()
}

/// Spawn a server that advertises `Content-Length: full_len` but sends only
/// `send_len` bytes before closing — simulating a mid-download interruption
/// (curl reports CURLE_PARTIAL_FILE). Serves up to `max_conns` connections so
/// every retry attempt sees the same truncation. Returns `(url, handle)`.
fn spawn_partial_server(
    full_len: usize,
    send_len: usize,
    max_conns: usize,
) -> (String, std::thread::JoinHandle<()>) {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local server");
    let port = listener.local_addr().unwrap().port();
    let body = vec![0xABu8; send_len];

    let handle = std::thread::spawn(move || {
        for _ in 0..max_conns {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut req_buf = [0u8; 4096];
                    let _ = stream.read(&mut req_buf);
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {full_len}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                    );
                    let _ = stream.write_all(header.as_bytes());
                    // Send fewer bytes than advertised, then drop the stream.
                    let _ = stream.write_all(&body);
                }
                Err(_) => break,
            }
        }
    });

    let url = format!("http://127.0.0.1:{port}/chromium.zip");
    (url, handle)
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

// ── happy path download + extract + binary executable ───────────

/// Happy path:
/// ChromiumDownloader::ensure() with a correct SHA-256 must download the
/// archive, verify SHA, extract the binary, and return Downloaded.
#[tokio::test]
async fn test_happy_path_download_and_extract() {
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
        .expect("ensure() must not return Err on valid archive");

    // Outcome must be Downloaded, not Skipped.
    assert!(
        matches!(
            outcome,
            loom_cli::chromium_downloader::DownloadOutcome::Downloaded(_)
        ),
        "expected Downloaded, got {:?}",
        outcome
    );

    // Binary must exist after extraction.
    let binary_path = install_dir.path().join(binary_subpath);
    assert!(
        binary_path.exists(),
        "binary must exist at {binary_path:?} after ensure()"
    );

    // Binary must be executable on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&binary_path)
            .unwrap()
            .permissions()
            .mode();
        assert!(
            mode & 0o111 != 0,
            "binary must be executable, mode={mode:o}"
        );
    }

    let _ = server.join();
}

// ── supply-chain mismatch → CliError::SupplyChain ───────────────

/// Tampered archive path:
/// ChromiumDownloader::ensure() with a wrong SHA-256 must return
/// CliError::SupplyChain (not exit 0, not CliError::Internal).
#[tokio::test]
async fn test_supply_chain_tampered_archive() {
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

    // Must be Err, and specifically CliError::SupplyChain.
    assert!(result.is_err(), "ensure() must return Err on SHA mismatch");

    match result.unwrap_err() {
        CliError::SupplyChain {
            expected_hash,
            actual_hash,
            ..
        } => {
            assert_eq!(
                expected_hash, wrong_sha,
                "SupplyChain::expected_hash must be the SHA we passed"
            );
            assert_ne!(
                actual_hash, wrong_sha,
                "SupplyChain::actual_hash must differ from the wrong SHA"
            );
        }
        other => panic!("expected CliError::SupplyChain, got: {other:?}"),
    }

    // Binary must NOT exist after a failed ensure (archive not extracted).
    let binary_path = install_dir.path().join(binary_subpath);
    assert!(
        !binary_path.exists(),
        "binary must not exist after supply-chain failure"
    );

    let _ = server.join();
}

// ── idempotence — second ensure() with matching sentinel → Skipped

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
    downloader
        .ensure(&url, &expected_sha)
        .await
        .expect("first ensure must succeed");

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

// ── progress reporting — ≥2 updates during the download (AC5) ────

/// AC5 machine-verifiable proxy: `ensure_with_progress` must fire the
/// `ProgressReporter` ≥2 times for a multi-chunk download, with a monotonic,
/// non-zero cumulative byte count that ends at the archive's full size.
#[tokio::test]
async fn test_progress_emits_at_least_two_updates() {
    use std::sync::{Arc, Mutex};

    // (bytes_done, total) tuples recorded by the progress reporter.
    type ProgressLog = Arc<Mutex<Vec<(u64, Option<u64>)>>>;

    let install_dir = TempDir::new().unwrap();
    let binary_subpath = "chromium";

    // 200 KiB stored (uncompressed) payload → archive spans ≥4 of the 64 KiB
    // PROGRESS_CHUNK_SIZE chunks, guaranteeing ≥2 progress callbacks.
    let payload = vec![0x5Au8; 200 * 1024];
    let zip_bytes = make_stored_zip(binary_subpath, &payload);
    assert!(
        zip_bytes.len() > 64 * 1024,
        "stored archive must exceed one chunk; got {} bytes",
        zip_bytes.len()
    );
    let expected_sha = sha256_hex(&zip_bytes);
    let archive_len = zip_bytes.len() as u64;

    let (url, server) = spawn_one_shot_server(zip_bytes);

    let downloader = ChromiumDownloader::new(ChromiumDownloaderConfig {
        install_dir: install_dir.path().to_path_buf(),
        binary_subpath: binary_subpath.into(),
    });

    let calls: ProgressLog = Arc::new(Mutex::new(Vec::new()));
    {
        let sink = calls.clone();
        let mut reporter = move |done: u64, total: Option<u64>| {
            sink.lock().unwrap().push((done, total));
        };
        let outcome = downloader
            .ensure_with_progress(&url, &expected_sha, &mut reporter)
            .await
            .expect("ensure_with_progress must succeed on a valid archive");
        assert!(
            matches!(
                outcome,
                loom_cli::chromium_downloader::DownloadOutcome::Downloaded(_)
            ),
            "expected Downloaded, got {outcome:?}"
        );
    }

    let calls = calls.lock().unwrap();
    assert!(
        calls.len() >= 2,
        "AC5 requires ≥2 progress updates; got {}",
        calls.len()
    );

    // Cumulative byte counts are strictly monotonic and end at the full size.
    let mut prev = 0u64;
    for (done, total) in calls.iter() {
        assert!(*done > prev, "progress must advance: {prev} -> {done}");
        prev = *done;
        // Streaming path advertises no total (curl-to-stdout has no length).
        assert_eq!(*total, None, "streaming download reports total=None");
    }
    assert_eq!(
        prev, archive_len,
        "final cumulative bytes must equal the archive size"
    );

    let _ = server.join();
}

// ── partial download → cleaned up, no corrupt binary left behind (AC5) ──

/// AC5 partial-download path: a server that closes mid-transfer makes every
/// retry fail. `ensure` must return a transport error (NOT `SupplyChain`),
/// leave no extracted binary, no sentinel, and no leftover temp file.
#[tokio::test]
async fn test_partial_download_cleaned_up() {
    let install_dir = TempDir::new().unwrap();
    let binary_subpath = "chromium";

    // Advertise 256 KiB but only ever send 16 KiB before closing — repeated
    // for every retry attempt so the whole bounded-retry loop is exercised.
    let (url, server) = spawn_partial_server(
        256 * 1024,
        16 * 1024,
        loom_cli::chromium_downloader::DOWNLOAD_MAX_ATTEMPTS,
    );

    let downloader = ChromiumDownloader::new(ChromiumDownloaderConfig {
        install_dir: install_dir.path().to_path_buf(),
        binary_subpath: binary_subpath.into(),
    });

    // SHA is irrelevant — the transfer never completes, so we never reach the
    // verify step. Use a syntactically valid (wrong) hash.
    let result = downloader.ensure(&url, &"0".repeat(64)).await;

    assert!(result.is_err(), "partial download must return Err");
    match result.unwrap_err() {
        CliError::Internal(msg) => {
            assert!(
                msg.contains("curl download failed") || msg.contains("download"),
                "expected a transport error, got: {msg}"
            );
        }
        other => panic!("expected CliError::Internal (transport), got: {other:?}"),
    }

    // No corrupt binary, no sentinel, no leftover temp file.
    assert!(
        !install_dir.path().join(binary_subpath).exists(),
        "no binary may be extracted from a partial download"
    );
    assert!(
        !install_dir.path().join(".archive_sha256").exists(),
        "no sentinel may be written for a failed download"
    );
    assert!(
        !install_dir.path().join(".chromium.download.tmp").exists(),
        "partial temp file must be cleaned up"
    );

    let _ = server.join();
}
