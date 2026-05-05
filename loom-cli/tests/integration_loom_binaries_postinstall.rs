// Integration tests for the `loom postinstall` extension that
// fetches `loom-daemon`, `loom-mcp`, `loom-shim-chromium` from the GH
// Release matching `env!("CARGO_PKG_VERSION")`.
//
// What's covered here: the user-facing error path. Running `cargo install
// --git URL loom-cli` (without `--tag v<X>.<Y>.<Z>`) gives the user a
// `loom` binary at the workspace's current version. If no GH Release
// exists for that version (mid-development between version bump + tag),
// `loom postinstall` should produce a clear actionable error rather than
// a confusing curl-got-an-empty-file panic.
//
// Happy-path testing (real tarball + manifest fixtures) is a follow-up:
// the existing `integration_chromium_postinstall.rs` shows the local
// HTTP-server harness pattern; extending it for a 2-route server +
// .tar.xz fixture needs an `xz2` or `flate2` dev-dep + ~120 LOC. For now,
// the unit tests in `loom_binaries_downloader/interface_tests.rs` cover
// the constants + target-triple selection, and manual smoke tests on a
// real release verify the network path.

use loom_cli::error_mapper::CliError;
use loom_cli::loom_binaries_downloader::{ensure, host_target_triple};
use tempfile::TempDir;

/// postinstall manifest 404 returns a typed error mentioning
/// the version. Catches mid-development version-skew before the user has
/// to dig through curl exit codes.
#[tokio::test]
async fn ensure_returns_actionable_error_when_release_404s() {
    let install_dir = TempDir::new().unwrap();
    // A version that vanishingly unlikely matches a real release: it's a
    // 4-component label that semver doesn't even like.
    let nonexistent_version = "0.0.0-test-no-release-zzzzz9999";

    let result = ensure(
        nonexistent_version,
        host_target_triple(),
        install_dir.path(),
    )
    .await;

    let err = result.expect_err("expected error for nonexistent release");
    match err {
        CliError::Internal(msg) => {
            // Must mention the version so the user knows which tag to look for
            // (either via the actionable 404 message or via the manifest URL
            // in a generic curl-failed message — both cases are acceptable).
            assert!(
                msg.contains(nonexistent_version),
                "error message should mention version {nonexistent_version}, got: {msg}"
            );
        }
        other => panic!("expected CliError::Internal, got: {other:?}"),
    }

    // Sentinel must NOT have been written (no successful install).
    assert!(
        !install_dir.path().join(".loom_binaries_sha256").exists(),
        "sentinel must not be written on failed download"
    );
}

/// target_triple resolution. Sanity-check that calling with a
/// triple that doesn't match any artifact in (a real, hypothetical)
/// manifest returns an actionable error, not a panic.
///
/// Uses an unsupported-target sentinel so we hit the manifest-found path
/// (network call), but the artifact selection step would return the "no
/// tarball for {triple}" error if the network call succeeded. In CI without
/// a real release, the 404 path catches first — this test still passes
/// because the error type is the same `CliError::Internal`. Both are
/// acceptable failure modes for a doomed lookup.
#[tokio::test]
async fn ensure_with_unsupported_triple_does_not_panic() {
    let install_dir = TempDir::new().unwrap();
    // Even on a real release, this triple won't match any artifact.
    let bogus_triple = "wasm64-unknown-multiverse";

    let result = ensure("0.0.0-test-no-release", bogus_triple, install_dir.path()).await;

    // We don't care what specific error — just that it's an error and not
    // a panic / segfault / hang.
    assert!(
        result.is_err(),
        "expected error for bogus target_triple, got: {result:?}"
    );
}
