// loom-host build.rs — embeds the SHA-256 of loom_surface_web.wasm at compile
// time so that load_one can verify artifact integrity at runtime.
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

    // Fold the RESOLVED wasmtime crate version into a build-time env so the
    // engine-compat hash (`WasmRuntime::precompile_compatibility_hash`) catches a
    // pure-wasmtime bump (source + opt-level unchanged) — a stale `.cwasm` that
    // would otherwise only fail at the late `deserialize_file`. Read from the
    // workspace `Cargo.lock` (the resolved, host-stable version string — NOT
    // compiled bytes, so the hash stays identical across macOS/Linux). Fail-soft
    // to "unknown" so a missing/odd lockfile never breaks the build.
    let wasmtime_version = wasmtime_version_from_lock(&workspace_root.join("Cargo.lock"))
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=LOOM_WASMTIME_VERSION={wasmtime_version}");

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
    // sha2 0.11 returns `digest::Array<u8, _>` (no `LowerHex` impl);
    // 0.10 returned `GenericArray<u8, _>` which did. Going through
    // `hex::encode` works for both representations.
    hex::encode(hasher.finalize())
}

/// Extract the resolved `wasmtime` version from a Cargo.lock. Returns the
/// `version = "X"` value of the first `[[package]]` block whose
/// `name = "wasmtime"`. `None` if the lockfile is unreadable or has no such
/// package (caller falls back to "unknown" — never panics).
fn wasmtime_version_from_lock(lock_path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(lock_path).ok()?;
    let mut in_wasmtime = false;
    for line in text.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            in_wasmtime = false;
            continue;
        }
        if line == r#"name = "wasmtime""# {
            in_wasmtime = true;
            continue;
        }
        if in_wasmtime {
            if let Some(rest) = line.strip_prefix("version = ") {
                return Some(rest.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}
