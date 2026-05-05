//! Regression guard for the FIX-INCOMPLETE class of bugs (registry id 1865):
//! the `loom-cli` binary previously embedded an 8-byte minimal-component
//! stub when `LOOM_SURFACE_WEB_WASM_PATH` was unset, so `cargo build
//! --release` produced a binary that AOT-compiled the stub and returned
//! garbage / wrong-format hashes from `loom action web.navigate`.
//!
//! These tests are NOT `#[ignore]`'d — they run on every `cargo test -p
//! loom-cli` and assert at the byte level that the embedded artifact is a
//! real WASM component when built in release mode. Without these, the
//! only fitness function for AC-SHA-04 lives behind `--ignored`, which
//! is what allowed the original regression.
//!
//! ## Build profile gating
//!
//! In dev/check builds (`PROFILE != release`), `loom-cli/build.rs` emits
//! the 8-byte stub by design (so `cargo check` works without the
//! wasm32-wasip2 toolchain). Tests that require real wasm bytes use the
//! `LOOM_CLI_EMBEDDED_PROFILE` env var (set by build.rs) to early-return
//! in non-release builds. `cargo test` (debug) skips them; `cargo test
//! --release` exercises them.
//!
//! ## Coverage map
//!
//! | Test | AC | What it catches |
//! |---|---|---|
//! | `embedded_wasm_is_not_stub_in_release` | AC-SHA-04 | (a) `LOOM_SURFACE_WEB_WASM_PATH` deliberately set to a stub-sized artifact, (b) future refactor reintroducing an in-band stub fallback. The recursive-cargo-failure case is covered by build.rs panicking; this test is the next line of defense. |
//! | `embedded_wasm_parses_as_component_in_release` | AC-SHA-04 | Bytes are a valid wasm component (catches a corrupted file or non-component module slipping through size check). |
//! | `embedded_matches_convention_path_in_release` | AC-WASMB-05 | Path-divergence regression: loom-cli's embedded bytes ≠ the convention-path artifact loom-host SHA-checks. Note: reads the convention path at test runtime; a parallel `cargo build` between this build and test could in theory race the read (file as follow-up if observed). |

use std::path::PathBuf;

const EMBEDDED_SURFACE_WEB: &[u8] = include_bytes!(env!("LOOM_CLI_EMBEDDED_SURFACE_WEB"));
const EMBEDDED_PROFILE: &str = env!("LOOM_CLI_EMBEDDED_PROFILE");

/// Returns true when build.rs ran in release mode (and therefore should
/// have produced real wasm). Returns false for dev/test profiles where
/// build.rs emits the stub by design.
fn is_release_build() -> bool {
    EMBEDDED_PROFILE == "release"
}

/// AC-SHA-04 regression guard: a release build MUST embed real wasm
/// bytes, not the 8-byte minimal-component stub or any plausibly-
/// truncated artifact. Real loom_surface_web.wasm is ~85KB; the stub
/// is 8 bytes; a 10KB lower bound is well above any stub and well
/// below any real build, so it captures the regression class without
/// being brittle to dep-version size variance.
///
/// What this catches in practice (post-A1):
///   1. `LOOM_SURFACE_WEB_WASM_PATH` deliberately set to a stub-sized
///      file (CI cache mishap, manual override misuse).
///   2. A future refactor reintroducing an in-band stub fallback in
///      release builds.
///
/// What this does NOT catch (covered elsewhere by build.rs panicking):
///   - Recursive cargo invocation failure (toolchain missing, lockfile
///     stale, etc.) — those abort the build before any test runs.
#[test]
fn embedded_wasm_is_not_stub_in_release() {
    if !is_release_build() {
        eprintln!(
            "skipping (PROFILE={EMBEDDED_PROFILE}); run with `cargo test --release` \
             to exercise the AC-SHA-04 regression guard"
        );
        return;
    }

    assert!(
        EMBEDDED_SURFACE_WEB.len() > 10_000,
        "AC-SHA-04: release build embedded a stub-sized or truncated wasm \
         ({} bytes; expected >10KB for real loom_surface_web.wasm). \
         The 8-byte MINIMAL_COMPONENT stub fails this check. \
         Investigate loom-cli/build.rs auto-build path, the \
         [profile.wasm-guest] section, and any LOOM_SURFACE_WEB_WASM_PATH \
         override that might be pointing at a stub artifact.",
        EMBEDDED_SURFACE_WEB.len()
    );
}

/// AC-SHA-04 regression guard: bytes must parse as a valid wasm
/// component (catches the case where the file is large but corrupted,
/// or a non-component module slipped through).
#[test]
fn embedded_wasm_parses_as_component_in_release() {
    if !is_release_build() {
        eprintln!("skipping (PROFILE={EMBEDDED_PROFILE})");
        return;
    }
    let mut validator = wasmparser::Validator::new_with_features(wasmparser::WasmFeatures::all());
    validator
        .validate_all(EMBEDDED_SURFACE_WEB)
        .unwrap_or_else(|e| {
            panic!(
                "AC-SHA-04: embedded loom_surface_web.wasm fails component \
                 validation: {e}. The file may be a stub, truncated, or \
                 built for the wrong target."
            )
        });
}

/// AC-WASMB-05 regression guard: the bytes loom-cli embeds and the
/// bytes loom-host computes the integrity SHA over MUST be the same.
/// Path-divergence (bug found by architect council review) silently
/// disables the integrity check when loom-cli writes to OUT_DIR but
/// loom-host reads from the convention path.
#[test]
fn embedded_matches_convention_path_in_release() {
    if !is_release_build() {
        eprintln!("skipping (PROFILE={EMBEDDED_PROFILE})");
        return;
    }
    use sha2::{Digest, Sha256};

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let workspace_root = PathBuf::from(&manifest_dir)
        .parent()
        .expect("workspace root")
        .to_path_buf();
    let convention_path = workspace_root.join("target/wasm32-wasip2/release/loom_surface_web.wasm");

    if !convention_path.exists() {
        // build.rs's convention-path copy is non-fatal (warns + continues).
        // If the file is missing here, surface as test failure: integrity
        // check is silently disabled in the resulting binary.
        panic!(
            "AC-WASMB-05: convention path missing at {}; \
             loom-cli/build.rs failed to copy the wasm artifact. \
             loom-host integrity check will be silently disabled.",
            convention_path.display()
        );
    }

    let convention_bytes = std::fs::read(&convention_path).unwrap_or_else(|e| {
        panic!(
            "failed to read convention-path wasm at {}: {}",
            convention_path.display(),
            e
        )
    });

    let embedded_sha = format!("{:x}", Sha256::digest(EMBEDDED_SURFACE_WEB));
    let convention_sha = format!("{:x}", Sha256::digest(&convention_bytes));

    assert_eq!(
        embedded_sha,
        convention_sha,
        "AC-WASMB-05: embedded loom_surface_web.wasm bytes diverge from the \
         convention-path artifact at {}. loom-host's integrity SHA will not \
         match the bytes loom-cli embedded. Check loom-cli/build.rs \
         convention-path-copy logic.",
        convention_path.display()
    );
}
