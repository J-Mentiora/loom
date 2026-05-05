// TDD tests for wasm-surface-build ACs covered by loom-cli:
// AC-WASMB-01 — wasm32 build produces artifact (requires toolchain; #[ignore])
// AC-WASMB-02 — compile_step AOTs .wasm → .cwasm + .sha256 sidecar

// postinstall-feature-gated imports (compile_step real impl requires loom-host)
#[cfg(feature = "postinstall")]
use loom_cli::postinstall_runner::{compile_step, StepOutcome};
#[cfg(feature = "postinstall")]
use tempfile::TempDir;

/// Minimal valid WASM component binary (empty component, component model layer 1).
/// Magic (\0asm) + version bytes encoding component-model.
#[cfg(feature = "postinstall")]
const MINIMAL_COMPONENT_BYTES: &[u8] = &[
    0x00, 0x61, 0x73, 0x6D, // magic: \0asm
    0x0D, 0x00, 0x01, 0x00, // version: component model
];

// ---------------------------------------------------------------------------
// AC-WASMB-01 — cargo build --target wasm32-wasip2 -p loom-surface-web
// ---------------------------------------------------------------------------
// Requires the wasm32-wasip2 target toolchain and a full Cargo build;
// gated behind LOOM_WASM_BUILD_TEST=1 to avoid CI failures when the
// toolchain is absent.
#[test]
#[ignore = "AC-WASMB-01: requires wasm32-wasip2 toolchain + LOOM_WASM_BUILD_TEST=1"]
fn test_ac_wasmb_01_wasm32_build_produces_artifact() {
    if std::env::var("LOOM_WASM_BUILD_TEST").as_deref() != Ok("1") {
        println!("LOOM_WASM_BUILD_TEST not set — test skipped");
        return;
    }

    // Find workspace root (CARGO_MANIFEST_DIR = .../loom-cli)
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let workspace_src = std::path::Path::new(&manifest_dir)
        .parent() // src/
        .unwrap()
        .to_path_buf();

    let status = std::process::Command::new("cargo")
        .args([
            "build",
            "--target",
            "wasm32-wasip2",
            "-p",
            "loom-surface-web",
            "--release",
        ])
        .current_dir(&workspace_src)
        .status()
        .expect("failed to spawn cargo build");

    assert!(
        status.success(),
        "AC-WASMB-01: cargo build for wasm32-wasip2 failed"
    );

    let artifact = workspace_src
        .parent() // projects/loom/
        .unwrap()
        .join("src/target/wasm32-wasip2/release/loom_surface_web.wasm");
    assert!(
        artifact.exists(),
        "AC-WASMB-01: artifact not found at {:?}",
        artifact
    );
}

// ---------------------------------------------------------------------------
// AC-WASMB-02 — compile_step writes .cwasm + .sha256 sidecar
// ---------------------------------------------------------------------------
// Gated behind the `postinstall` cargo feature because compile_step's real
// implementation (the one that calls loom_host::compiler::Compiler) is
// only compiled when `postinstall` is enabled. Without the feature the
// function returns StepOutcome::Skipped (BC-CLI-01 isolation guard).
//
// Run with: cargo test -p loom-cli --test ac_wasmb_postinstall --features postinstall
#[cfg(feature = "postinstall")]
#[test]
fn test_ac_wasmb_02_compile_step_writes_cwasm() {
    // Point LOOM_WASM_DIR at a tempdir containing a minimal .wasm component.
    let wasm_src_dir = TempDir::new().expect("wasm_src_dir tempdir");
    let wasm_path = wasm_src_dir.path().join("loom_surface_web.wasm");
    std::fs::write(&wasm_path, MINIMAL_COMPONENT_BYTES).expect("write minimal wasm");

    // SAFETY: test-only, single-threaded section around env mutation.
    // We set and restore LOOM_WASM_DIR to avoid polluting sibling tests.
    let prev = std::env::var("LOOM_WASM_DIR").ok();
    std::env::set_var("LOOM_WASM_DIR", wasm_src_dir.path());

    let surfaces_dir = TempDir::new().expect("surfaces_dir tempdir");
    let result = compile_step(surfaces_dir.path());

    // Restore env before any assertions so cleanup runs even on panic.
    match prev {
        Some(v) => std::env::set_var("LOOM_WASM_DIR", v),
        None => std::env::remove_var("LOOM_WASM_DIR"),
    }

    let outcomes = result.expect("compile_step returned Err");

    // Assert: at least one Compiled outcome (not just Skipped).
    let has_compiled = outcomes
        .iter()
        .any(|o| matches!(o, StepOutcome::Compiled(_)));
    assert!(
        has_compiled,
        "AC-WASMB-02: expected StepOutcome::Compiled, got: {:?}",
        outcomes
    );

    // Assert: .cwasm file exists in surfaces_dir.
    let cwasm = surfaces_dir.path().join("loom_surface_web.cwasm");
    assert!(
        cwasm.exists(),
        "AC-WASMB-02: loom_surface_web.cwasm not found in surfaces_dir"
    );

    // Assert: .sha256 sidecar exists next to the .cwasm.
    let sidecar = surfaces_dir.path().join("loom_surface_web.sha256");
    assert!(
        sidecar.exists(),
        "AC-WASMB-02: loom_surface_web.sha256 sidecar not found"
    );

    // Assert: sidecar is non-empty hex string.
    let sha_content = std::fs::read_to_string(&sidecar).expect("read sidecar");
    assert!(
        !sha_content.trim().is_empty(),
        "AC-WASMB-02: SHA-256 sidecar must not be empty"
    );
    assert_eq!(
        sha_content.trim().len(),
        64,
        "AC-WASMB-02: SHA-256 sidecar must be 64 hex chars, got {}",
        sha_content.trim().len()
    );
}
