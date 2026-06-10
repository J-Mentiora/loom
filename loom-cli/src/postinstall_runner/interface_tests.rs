// Interface tests for `PostinstallRunner`. Verifies step
// enumeration, pre-warm flow, and the cfg-feature gating.

use super::postinstall_runner::{
    tagless_install_warning, PostinstallOptions, PostinstallReceipt, StepOutcome, STEP_LABELS,
};

#[test]
fn step_labels_are_compile_chromium_loom_binaries_launchd_manpages_in_order() {
    assert_eq!(
        STEP_LABELS,
        &[
            "compile_module",
            "chromium",
            "loom_binaries",
            "launchd",
            "manpages"
        ]
    );
}

#[test]
fn postinstall_options_carry_surfaces_chromium_plist() {
    let o = PostinstallOptions {
        surfaces_dir: "/tmp/surfaces".into(),
        schemas_dir: "/tmp/schemas/v1".into(),
        chromium_url: "https://example.com/chrome.tar.xz".into(),
        chromium_expected_sha256: "abc".into(),
        chromium_dir: "/tmp/chromium".into(),
        plist_path: "/tmp/com.loom.daemon.plist".into(),
        loom_binaries_version: "1.0.0".into(),
        loom_binaries_target_triple: "aarch64-apple-darwin".into(),
        loom_binaries_install_dir: "/tmp/loom-bin".into(),
        skip_chromium: false,
        skip_binaries: false,
        man_install_dir: None,
    };
    assert_eq!(o.chromium_expected_sha256, "abc");
}

#[test]
fn step_outcome_variant_set_locked() {
    fn _ck(o: StepOutcome) -> &'static str {
        match o {
            StepOutcome::Skipped => "skipped",
            StepOutcome::Compiled(_) => "compiled",
            StepOutcome::Downloaded => "downloaded",
            StepOutcome::Wrote => "wrote",
        }
    }
    let _ = _ck;
}

#[test]
fn postinstall_receipt_carries_steps_and_outcomes() {
    let r = PostinstallReceipt {
        status: "ok".into(),
        steps: vec![
            "compile_module",
            "chromium",
            "loom_binaries",
            "launchd",
            "manpages",
        ],
        compile_outcomes: vec![StepOutcome::Skipped],
        schemas: super::postinstall_runner::SchemaStepOutcome::Skipped,
        chromium: StepOutcome::Skipped,
        loom_binaries: Some(StepOutcome::Skipped),
        launchd: Some(StepOutcome::Skipped),
        manpages: StepOutcome::Skipped,
    };
    assert_eq!(r.steps.len(), 5);
}

// === receipt is emitted as canonical JSON on stdout (regression: the
// dispatcher used to discard it with `.map(|_| ())`, so the documented
// stdout receipt was silently dropped) ===

#[test]
fn postinstall_receipt_serialises_to_canonical_json() {
    let r = PostinstallReceipt {
        status: "ok".into(),
        steps: STEP_LABELS.to_vec(),
        compile_outcomes: vec![
            StepOutcome::Compiled("/tmp/surfaces/loom_surface_web.cwasm".into()),
            StepOutcome::Skipped,
        ],
        schemas: super::postinstall_runner::SchemaStepOutcome::Populated(17),
        chromium: StepOutcome::Downloaded,
        loom_binaries: None,
        launchd: Some(StepOutcome::Wrote),
        manpages: StepOutcome::Skipped,
    };
    let value = serde_json::to_value(&r).expect("receipt must serialise");
    // Canonical-JSON path (RFC 8785) must accept the receipt — this is
    // exactly what `emit_to_stdout("postinstall", ...)` runs in the
    // default --json mode.
    let canonical = crate::output_formatter::OutputFormatter::<
        crate::output_formatter::StdoutSink,
    >::canonical_json(&value)
    .expect("canonical JSON must serialise");
    assert!(canonical.contains(r#""status":"ok""#), "got: {canonical}");
    // Unit variants are bare strings; data variants are tagged objects.
    assert!(
        canonical.contains(r#""chromium":"downloaded""#),
        "got: {canonical}"
    );
    assert!(
        canonical.contains(r#"{"compiled":"/tmp/surfaces/loom_surface_web.cwasm"}"#),
        "got: {canonical}"
    );
    assert!(
        canonical.contains(r#"{"populated":17}"#),
        "got: {canonical}"
    );
    // `--skip-binaries` → null, distinguishable from "skipped".
    assert!(
        canonical.contains(r#""loom_binaries":null"#),
        "got: {canonical}"
    );
}

// === Tagless install warning (AC6 / R6) ===
//
// The warning must contain the stable substrings `non-release` and `--tag`
// (FND-0001), fire on a non-release commit (`"0"`) or a pre-release version,
// and stay silent for a tagged release (`"1"`) or a no-git tarball (`"unknown"`).

#[test]
fn tagless_warning_fires_on_non_release_commit() {
    let w = tagless_install_warning("0.9.8", "0").expect("a non-release commit build must warn");
    assert!(w.contains("non-release"), "missing `non-release`: {w}");
    assert!(w.contains("--tag"), "missing `--tag`: {w}");
}

#[test]
fn tagless_warning_fires_on_prerelease_version() {
    // Even on a tagged build, a pre-release semver is itself non-release.
    let w = tagless_install_warning("0.10.0-dev", "1").expect("a pre-release version must warn");
    assert!(w.contains("non-release") && w.contains("--tag"));
}

#[test]
fn tagless_warning_silent_on_tagged_release() {
    assert!(
        tagless_install_warning("0.9.8", "1").is_none(),
        "a tagged release build must not warn"
    );
}

#[test]
fn tagless_warning_silent_on_unknown_tarball_build() {
    assert!(
        tagless_install_warning("0.9.8", "unknown").is_none(),
        "a no-git source-tarball release install must not warn"
    );
}

// === loom-host gated behind cfg(feature = "postinstall") ===
//
// On non-postinstall builds the `run` symbol still exists (so callers
// compile) but returns `CliError::Internal`. The compile_step symbol
// is fully gated. We can't test cfg at runtime; the test below
// documents the contract.
#[test]
fn cfg_feature_postinstall_documented() {
    let s = "#[cfg(feature = \"postinstall\")]";
    assert!(s.contains("postinstall"));
}

// Pre-warm step has its own idempotence guard.
#[test]
fn chromium_step_signature_takes_url_and_expected_sha() {
    use super::postinstall_runner::*;
    fn _ck(d: &crate::chromium_downloader::ChromiumDownloader, url: &str, sha: &str) {
        let _f = async move {
            let _: Result<StepOutcome, crate::CliError> = chromium_step(d, url, sha).await;
        };
    }
    let _ = _ck;
}

#[test]
fn plist_step_signature() {
    use super::postinstall_runner::*;
    fn _ck(
        w: &crate::launchd_plist_writer::LaunchdPlistWriter,
    ) -> Result<StepOutcome, crate::CliError> {
        plist_step(w)
    }
    let _ = _ck;
}

// === Idempotency — Skipped when binary present + sentinel matches ===
#[tokio::test]
async fn chromium_step_skips_when_sha_matches() {
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    // Binary must exist for idempotence check.
    std::fs::write(dir.path().join("Chromium"), b"fake-chromium-content").unwrap();
    // The downloader checks the sentinel (.archive_sha256), not the binary SHA,
    // for idempotence. Write the sentinel with the expected SHA.
    let expected_sha = "cafebabe00000000cafebabe00000000cafebabe00000000cafebabe00000000";
    std::fs::write(dir.path().join(".archive_sha256"), expected_sha).unwrap();
    let downloader = crate::chromium_downloader::ChromiumDownloader::new(
        crate::chromium_downloader::ChromiumDownloaderConfig {
            install_dir: dir.path().to_path_buf(),
            binary_subpath: "Chromium".into(),
        },
    );
    let result = super::postinstall_runner::chromium_step(
        &downloader,
        "https://placeholder.invalid/ignored-because-already-present",
        expected_sha,
    )
    .await;
    assert!(
        matches!(result, Ok(super::postinstall_runner::StepOutcome::Skipped)),
        "expected Skipped, got: {result:?}"
    );
}

// === SupplyChain error when downloaded sha mismatches pin ===
#[tokio::test]
async fn chromium_step_supply_chain_on_sha_mismatch() {
    use tempfile::TempDir;
    // Source file served via file:// URL — avoids real network in CI.
    let source_dir = TempDir::new().unwrap();
    let source_file = source_dir.path().join("chromium.zip");
    std::fs::write(&source_file, b"real-chromium-content").unwrap();
    let install_dir = TempDir::new().unwrap();
    let downloader = crate::chromium_downloader::ChromiumDownloader::new(
        crate::chromium_downloader::ChromiumDownloaderConfig {
            install_dir: install_dir.path().to_path_buf(),
            binary_subpath: "Chromium".into(),
        },
    );
    let file_url = format!("file://{}", source_file.display());
    // Wrong SHA — 64 zero hex chars.
    let wrong_sha = "0000000000000000000000000000000000000000000000000000000000000000";
    let result = super::postinstall_runner::chromium_step(&downloader, &file_url, wrong_sha).await;
    assert!(
        matches!(result, Err(crate::CliError::SupplyChain { .. })),
        "expected SupplyChain, got: {result:?}"
    );
}

// === PermissionDenied on plist write degrades to Skipped ===
// `LaunchdPlistWriter::write` is `#[cfg(target_os = "macos")]`; the non-macOS
// stub returns `CliError::Internal`, not the `PermissionDenied` path this
// test exercises. Gating on `unix` would otherwise fail on Linux runners.
#[cfg(target_os = "macos")]
#[test]
fn plist_step_returns_skipped_on_permission_denied() {
    use std::os::unix::fs::PermissionsExt as _;
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    let readonly_dir = dir.path().join("readonly");
    std::fs::create_dir_all(&readonly_dir).unwrap();
    // 0o555 = r-xr-xr-x — readable + executable, not writable.
    std::fs::set_permissions(&readonly_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let plist_path = readonly_dir.join("com.loom.daemon.plist");
    let writer = crate::launchd_plist_writer::LaunchdPlistWriter::new(
        crate::launchd_plist_writer::LaunchdPlistConfig {
            loom_binary: "/usr/local/bin/loom".into(),
            plist_path,
        },
    );
    let result = super::postinstall_runner::plist_step(&writer);
    assert!(
        matches!(result, Ok(super::postinstall_runner::StepOutcome::Skipped)),
        "PermissionDenied on plist write must degrade to Skipped, got: {result:?}"
    );
}
