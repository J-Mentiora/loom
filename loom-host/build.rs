// loom-host build.rs — embeds the SHA-256 of loom_surface_web.wasm at compile
// time so that load_one can verify artifact integrity at runtime (AC-WASMB-05).
//
// The wasm artifact is produced by `loom-cli/build.rs` (which auto-builds
// the wasm32-wasip2 cdylib in release builds and copies it to the workspace
// convention path `target/wasm32-wasip2/release/loom_surface_web.wasm`).
// This build script reads from that convention path, so it will find the
// artifact whenever loom-cli has been built in release mode.
//
// If the wasm artifact does not exist (dev builds before loom-cli has run,
// or workspace `cargo build -p loom-host` without loom-cli), emits an empty
// string and the integrity check is skipped at runtime.

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let workspace_root = std::path::Path::new(&manifest_dir)
        .parent()
        .expect("loom-host manifest has no parent dir");

    let wasm_path = std::env::var("LOOM_SURFACE_WEB_WASM_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            workspace_root.join("target/wasm32-wasip2/release/loom_surface_web.wasm")
        });

    if wasm_path.exists() {
        let bytes = std::fs::read(&wasm_path)
            .unwrap_or_else(|e| panic!("failed to read {:?}: {}", wasm_path, e));
        let sha = sha256_hex(&bytes);
        println!("cargo:rustc-env=LOOM_SURFACE_WEB_SHA256={}", sha);
    } else {
        // Dev build without the wasm artifact — emit empty string; the runtime
        // check in load_one skips the comparison when the expected SHA is empty.
        println!("cargo:rustc-env=LOOM_SURFACE_WEB_SHA256=");
    }

    // Re-run triggers — surface-web sources, manifest, WIT, and Cargo.lock
    // all influence the wasm bytes (and therefore the SHA we embed).
    // Meaningful because loom-cli/build.rs writes to the convention path.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=LOOM_SURFACE_WEB_WASM_PATH");
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
    if wasm_path.exists() {
        println!("cargo:rerun-if-changed={}", wasm_path.display());
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}
