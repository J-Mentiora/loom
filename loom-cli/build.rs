// loom-cli build script — produces and embeds the WASM surface bytes for
// AC-PICOMP-04 / AC-SHA-04. The embedded artifact is read by
// `postinstall_runner` via `include_bytes!(env!("LOOM_CLI_EMBEDDED_SURFACE_WEB"))`
// during `loom postinstall`'s compile_step.
//
// ## Behavior matrix
//
// | PROFILE  | LOOM_SURFACE_WEB_WASM_PATH | feature vendored-wasm | LOOM_SKIP_WASM_BUILD | Outcome |
// |----------|----------------------------|------------------------|----------------------|---------|
// | release  | set                        | -                      | -                    | Copy supplied artifact (CI / explicit override) |
// | release  | unset                      | on (default)           | -                    | Copy committed `vendor/loom_surface_web.wasm` (AC-DIST-01) |
// | release  | unset                      | off                    | unset                | Auto-build wasm32-wasip2 cdylib via recursive cargo |
// | release  | unset                      | off                    | =1                   | Panic — release builds refuse the stub |
// | debug    | set                        | -                      | -                    | Copy supplied artifact |
// | debug    | unset                      | -                      | -                    | Emit 8-byte minimal-component stub + cargo:warning |
//
// ## Recursive cargo invocation
//
// When auto-building, we shell out to `cargo build --target wasm32-wasip2
// -p loom-surface-web --profile=wasm-guest`. To avoid the workspace
// cargo-lock contention, the child invocation uses
// `--target-dir <workspace>/target/wasm-guest/` — separate `.cargo-lock`
// from the parent's `target/.cargo-lock`. The custom `wasm-guest` profile
// (defined in workspace Cargo.toml) overrides `[profile.release]`'s
// `lto="fat"` / `codegen-units=1` because wasmtime AOT-compiles the cdylib
// at postinstall time anyway, so fat-LTO on the guest is wasted work.
//
// ## Environment hygiene
//
// The recursive `cargo` Command inherits the parent process env. We
// explicitly remove vars that would mis-target the child or break wasm
// linking: `RUSTFLAGS` (host link flags), `CARGO_TARGET_DIR` (explicit
// `--target-dir` flag wins, but the env still leaks to nested build scripts),
// `RUSTC_WRAPPER` (sccache cache-key collisions on wasm32-wasip2),
// `CARGO_BUILD_TARGET`, `CARGO_ENCODED_RUSTFLAGS`, `CARGO_PRIMARY_PACKAGE`.
//
// ## Path divergence fix
//
// We also copy the produced artifact to the workspace convention path
// `<workspace>/target/wasm32-wasip2/release/loom_surface_web.wasm` so
// `loom-host/build.rs` can compute its integrity SHA (AC-WASMB-05).
//
// ## Re-run triggers
//
// `cargo:rerun-if-changed` watches the surface-web source tree, its
// Cargo.toml, the WIT contract, and the workspace Cargo.lock. Edits to
// any of these invalidate the embedded bytes.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const MINIMAL_COMPONENT: &[u8] = &[
    0x00, 0x61, 0x73, 0x6D, // magic: \0asm
    0x0D, 0x00, 0x01, 0x00, // version: component model
];

fn main() {
    emit_build_provenance();

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR not set by Cargo"));
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"));
    let workspace_root = manifest_dir
        .parent()
        .expect("loom-cli manifest has no parent dir")
        .to_path_buf();
    let dest = out_dir.join("loom_surface_web.wasm");

    // Re-run triggers — declare unconditionally so cargo invalidates the
    // build script when any of these change, regardless of which branch
    // we take below.
    println!("cargo:rerun-if-env-changed=LOOM_SURFACE_WEB_WASM_PATH");
    println!("cargo:rerun-if-env-changed=LOOM_SKIP_WASM_BUILD");
    println!("cargo:rerun-if-env-changed=PROFILE");
    println!("cargo:rerun-if-env-changed=CARGO_NET_OFFLINE");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_VENDORED_WASM");
    // Vendored-wasm path: watch the committed artifact so feature-on rebuilds
    // pick up updates from `just vendor-wasm`.
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("vendor/loom_surface_web.wasm").display()
    );
    // Directory watch catches new/deleted files; explicit file watches add
    // belt-and-suspenders coverage for the most-edited source file in case
    // a future cargo version changes directory-recursion semantics.
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join("loom-surface-web/src").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join("loom-surface-web/src/lib.rs").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join("loom-surface-web/Cargo.toml").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join("wit/loom-surface.wit").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join("Cargo.lock").display()
    );

    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    let is_release = profile == "release";

    // Surface the build-time profile to the test crate so non-ignored
    // regression tests in tests/embedded_wasm_sha.rs can gate the strict
    // size + component-parse assertions to release builds (the dev path
    // emits a stub by design, see "Behavior matrix" above).
    println!("cargo:rustc-env=LOOM_CLI_EMBEDDED_PROFILE={}", profile);

    let src_path: PathBuf = if let Ok(explicit) = std::env::var("LOOM_SURFACE_WEB_WASM_PATH") {
        let p = PathBuf::from(explicit);
        println!("cargo:rerun-if-changed={}", p.display());
        p
    } else if !is_release {
        // Dev / cargo-check / clippy / rust-analyzer path: emit the 8-byte
        // stub so build doesn't require the wasm32-wasip2 toolchain. Real
        // wasm only matters for release builds (which actually run the
        // postinstall AOT compile).
        std::fs::write(&dest, MINIMAL_COMPONENT).unwrap_or_else(|e| {
            panic!(
                "build.rs: failed to write minimal stub to {}: {}",
                dest.display(),
                e
            )
        });
        println!(
            "cargo:warning=loom_surface_web wasm is a dev stub (PROFILE={profile}); \
             run `cargo build --release` for the real artifact"
        );
        println!(
            "cargo:rustc-env=LOOM_CLI_EMBEDDED_SURFACE_WEB={}",
            dest.display()
        );
        return;
    } else if std::env::var("CARGO_FEATURE_VENDORED_WASM").is_ok() {
        // AC-DIST-01: `vendored-wasm` (default) reads the committed artifact
        // so `cargo install --git ... loom-cli` succeeds on a fresh machine
        // without requiring `rustup target add wasm32-wasip2`. CI's
        // `vendored-wasm-check` job re-builds from source and diffs against
        // this file to catch staleness.
        let vendored = manifest_dir.join("vendor/loom_surface_web.wasm");
        if !vendored.exists() {
            panic!(
                "build.rs: vendored-wasm feature is on but {} is missing. \
                 Run `just vendor-wasm` to regenerate, or build with \
                 `--no-default-features --features postinstall` to fall back to \
                 the recursive cargo build path.",
                vendored.display()
            );
        }
        vendored
    } else if std::env::var("LOOM_SKIP_WASM_BUILD").as_deref() == Ok("1") {
        // Explicit opt-out at release: refuse. Silent-stub-in-release is
        // the failure mode that produced the FIX-INCOMPLETE retest.
        panic!(
            "build.rs: LOOM_SKIP_WASM_BUILD=1 set during release build. \
             Release builds must embed the real wasm artifact. \
             Either unset LOOM_SKIP_WASM_BUILD, or set LOOM_SURFACE_WEB_WASM_PATH \
             to a prebuilt wasm."
        );
    } else {
        build_wasm_guest(&workspace_root)
    };

    std::fs::copy(&src_path, &dest).unwrap_or_else(|e| {
        panic!(
            "build.rs: failed to copy {} -> {}: {}",
            src_path.display(),
            dest.display(),
            e
        )
    });

    // Path-divergence fix (council C2): also copy to the convention path
    // so loom-host/build.rs can compute the integrity SHA (AC-WASMB-05).
    let convention_path = workspace_root.join("target/wasm32-wasip2/release/loom_surface_web.wasm");
    if let Some(parent) = convention_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::copy(&src_path, &convention_path) {
        // Non-fatal: integrity check stays empty if this fails. Warn for visibility.
        println!(
            "cargo:warning=failed to copy wasm to convention path {}: {}",
            convention_path.display(),
            e
        );
    }

    println!(
        "cargo:rustc-env=LOOM_CLI_EMBEDDED_SURFACE_WEB={}",
        dest.display()
    );
}

/// Emit `LOOM_GIT_SHA` and `LOOM_BUILD_DATE` for the version-command crate
/// to consume via `env!()` (AC-VER-02). Both fall back to "unknown" so
/// cargo-dist source-tarball builds (no `.git` directory) still compile.
fn emit_build_provenance() {
    let sha = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    // YYYY-MM-DD UTC, hand-formatted from the unix epoch so we don't pull in chrono.
    // SOURCE_DATE_EPOCH (reproducible-builds.org) is honored if set.
    let secs: u64 = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.parse().ok())
        .or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs())
        })
        .unwrap_or(0);
    let date = format_ymd_utc(secs);

    println!("cargo:rustc-env=LOOM_GIT_SHA={sha}");
    println!("cargo:rustc-env=LOOM_BUILD_DATE={date}");

    // Re-run when HEAD moves so the SHA stays fresh on `cargo build`.
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs/heads");
}

/// Format a unix timestamp as YYYY-MM-DD in UTC. Civil-from-days algorithm
/// (Howard Hinnant's date library, public domain). No chrono dependency.
fn format_ymd_utc(secs: u64) -> String {
    if secs == 0 {
        return "unknown".to_string();
    }
    let days = (secs / 86_400) as i64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// Recursively invoke cargo to build the wasm32-wasip2 cdylib. Returns
/// the output path. Panics on failure with actionable guidance.
fn build_wasm_guest(workspace_root: &Path) -> PathBuf {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let target_dir = workspace_root.join("target/wasm-guest");

    let mut cmd = Command::new(&cargo);
    cmd.arg("build")
        .arg("--target")
        .arg("wasm32-wasip2")
        .arg("-p")
        .arg("loom-surface-web")
        .arg("--profile")
        .arg("wasm-guest")
        .arg("--target-dir")
        .arg(&target_dir)
        .arg("--manifest-path")
        .arg(workspace_root.join("Cargo.toml"))
        .arg("--locked");

    // Propagate --offline if the parent build is offline.
    if std::env::var("CARGO_NET_OFFLINE").as_deref() == Ok("true") {
        cmd.arg("--offline");
    }

    // Env hygiene: the parent's env can poison the recursive build.
    // - CARGO_TARGET_DIR: explicit --target-dir wins, but env still leaks to nested scripts.
    // - CARGO_BUILD_TARGET: would override --target.
    // - CARGO_ENCODED_RUSTFLAGS / RUSTFLAGS: host-target link flags break wasm linking.
    // - RUSTC_WRAPPER: sccache cache-key collisions on wasm32-wasip2.
    // - CARGO_PRIMARY_PACKAGE: leaked from parent breaks the child's primary-package detection.
    cmd.env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_BUILD_TARGET")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("CARGO_PRIMARY_PACKAGE")
        .env_remove("RUSTC")
        .env_remove("RUSTC_BOOTSTRAP");

    let status = cmd.status().unwrap_or_else(|e| {
        panic!(
            "build.rs: failed to spawn `{} build --target wasm32-wasip2 -p loom-surface-web`: {}\n\
             - Run `rustup target add wasm32-wasip2` if the target is missing.\n\
             - Or set LOOM_SURFACE_WEB_WASM_PATH=<path> to a prebuilt wasm.",
            cargo, e
        )
    });

    if !status.success() {
        panic!(
            "build.rs: `{} build --target wasm32-wasip2 -p loom-surface-web --profile=wasm-guest` \
             failed (exit {}).\n\
             Likely causes:\n\
             - wasm32-wasip2 target not installed → run `rustup target add wasm32-wasip2`.\n\
             - Cargo.lock out of sync (--locked refuses) → run `cargo update` from the workspace root.\n\
             - Network unavailable in --offline mode → unset CARGO_NET_OFFLINE.\n\
             Workarounds:\n\
             - Set LOOM_SURFACE_WEB_WASM_PATH=<path> to a prebuilt wasm.\n\
             - Re-run with -vv for full child cargo output.",
            cargo,
            status.code().unwrap_or(-1)
        );
    }

    target_dir.join("wasm32-wasip2/wasm-guest/loom_surface_web.wasm")
}
