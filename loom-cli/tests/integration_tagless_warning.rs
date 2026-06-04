//! AC6 / R6 — a tagless / non-release install triggers a precise postinstall
//! warning whose text contains the stable substrings `non-release` and `--tag`
//! (FND-0001).
//!
//! Hermetic by design: this exercises the pure `tagless_install_warning`
//! decision function directly — no daemon, no network, no subprocess, no git
//! state dependence — so the AC contract holds deterministically regardless of
//! the CI checkout's commit/tag layout (no-flake bar, FND-0006). The build-time
//! `LOOM_RELEASE_BUILD` marker that feeds this function in production is
//! produced by `loom-cli/build.rs`; here we feed synthetic markers so each
//! branch of the contract is asserted explicitly.

use loom_cli::postinstall_runner::tagless_install_warning;

/// A non-release commit (the tagless `cargo install --git` path) warns, and the
/// message carries the load-bearing substrings the PRD pins.
#[test]
fn non_release_commit_warns_with_tag_guidance() {
    let msg = tagless_install_warning("0.9.8", "0")
        .expect("a non-release commit build must emit the tagless warning");
    assert!(
        msg.contains("non-release"),
        "warning must contain `non-release`: {msg}"
    );
    assert!(msg.contains("--tag"), "warning must contain `--tag`: {msg}");
}

/// A pre-release semver is non-release even on an otherwise tagged build.
#[test]
fn prerelease_version_warns() {
    let msg = tagless_install_warning("0.10.0-dev.3", "1")
        .expect("a pre-release semver must emit the tagless warning");
    assert!(msg.contains("non-release") && msg.contains("--tag"));
}

/// A clean version built exactly on a release tag is silent.
#[test]
fn tagged_release_is_silent() {
    assert!(
        tagless_install_warning("0.9.8", "1").is_none(),
        "a tagged release build must not warn"
    );
}

/// A cargo-dist source-tarball install (no `.git` at build time → marker
/// `unknown`) is a legitimate release and must not be false-flagged.
#[test]
fn source_tarball_unknown_is_silent() {
    assert!(
        tagless_install_warning("0.9.8", "unknown").is_none(),
        "a source-tarball (no-git) release install must not warn"
    );
}
