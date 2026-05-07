// TDD tests for the wasm-host feature acceptance criteria
// (the platform-version gate lives in wasm_host::tests).

use std::path::Path;
use std::process::Command;

fn loom_src_root() -> std::path::PathBuf {
    // CARGO_MANIFEST_DIR = <workspace>/loom-host. The workspace root
    // (one level up) is the location of Cargo.toml + scripts/ +
    // security/. The historical pipeline layout had an extra `src/`
    // hop because loom lived under `projects/loom/src/`; that's gone
    // post-extraction.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    Path::new(&manifest_dir)
        .parent() // <workspace> root
        .unwrap()
        .to_path_buf()
}

// ---------------------------------------------------------------------------
// Linux compile gate
// ---------------------------------------------------------------------------
// The actual cross-compile is CI work; this test pins that the crate declares
// no OS-conditional compilation that would break on Linux.  It scans for
// `#[cfg(target_os = "macos")]` blocks OUTSIDE the dedicated platform-check
// module (wasm_host) — any such block in other modules would signal an
// inadvertent macOS-only code path.
#[test]
fn test_linux_compile_gate_no_macos_cfg_outside_platform_module() {
    let src_dir = loom_src_root().join("loom-host/src");
    let mut violations: Vec<String> = Vec::new();
    scan_for_pattern(
        &src_dir,
        r#"cfg(target_os = "macos")"#,
        &["wasm_host"],
        &mut violations,
    );
    assert!(
        violations.is_empty(),
        "found macOS-cfg outside wasm_host platform module:\n{}",
        violations.join("\n")
    );
}

// ---------------------------------------------------------------------------
// No Firefox / WebKit code paths
// ---------------------------------------------------------------------------
#[test]
fn test_no_firefox_webkit_symbols_in_source() {
    let src_dir = loom_src_root().join("loom-host/src");
    let banned = ["firefox", "gecko", "webkit", "safari"];
    let mut violations: Vec<String> = Vec::new();
    for term in &banned {
        scan_for_pattern_insensitive(&src_dir, term, &[], &mut violations);
    }
    assert!(
        violations.is_empty(),
        "found browser-engine symbols in loom-host source:\n{}",
        violations.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Single binary, no language runtime deps
// ---------------------------------------------------------------------------
// Pins that reqwest is configured with rustls-tls (no openssl / native-tls).
// loom-host uses `reqwest = { workspace = true }` so the feature spec lives
// in the workspace Cargo.toml.
#[test]
fn test_single_binary_no_runtime_deps_reqwest_uses_rustls() {
    let workspace_toml_path = loom_src_root().join("Cargo.toml");
    let workspace_toml =
        std::fs::read_to_string(&workspace_toml_path).expect("failed to read workspace Cargo.toml");
    assert!(
        workspace_toml.contains("rustls-tls"),
        "reqwest must declare rustls-tls feature in workspace Cargo.toml"
    );
    assert!(
        !workspace_toml.contains("native-tls"),
        "found native-tls in workspace Cargo.toml — must use rustls only"
    );
}

// ---------------------------------------------------------------------------
// Adapter isolation linter passes
// ---------------------------------------------------------------------------
#[test]
fn test_adapter_isolation_linter_passes() {
    let script_path = loom_src_root().join("scripts/lint_no_platform_imports.py");
    assert!(
        script_path.exists(),
        "scripts/lint_no_platform_imports.py does not exist at {:?}",
        script_path
    );
    let status = Command::new("python3")
        .arg(&script_path)
        .status()
        .expect("failed to run lint_no_platform_imports.py");
    assert!(
        status.success(),
        "lint_no_platform_imports.py exited with: {:?}",
        status
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn scan_for_pattern(dir: &Path, pattern: &str, excluded_subdirs: &[&str], out: &mut Vec<String>) {
    scan_for_pattern_impl(dir, pattern, excluded_subdirs, false, out);
}

fn scan_for_pattern_insensitive(
    dir: &Path,
    pattern: &str,
    excluded_subdirs: &[&str],
    out: &mut Vec<String>,
) {
    scan_for_pattern_impl(dir, pattern, excluded_subdirs, true, out);
}

fn scan_for_pattern_impl(
    dir: &Path,
    pattern: &str,
    excluded_subdirs: &[&str],
    case_insensitive: bool,
    out: &mut Vec<String>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if excluded_subdirs.contains(&name) {
                continue;
            }
            scan_for_pattern_impl(&path, pattern, excluded_subdirs, case_insensitive, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs")
            && !path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.contains("test"))
                .unwrap_or(false)
        {
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            for (i, line) in content.lines().enumerate() {
                // Skip comment lines and doc comments
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") {
                    continue;
                }
                let matches = if case_insensitive {
                    line.to_lowercase().contains(&pattern.to_lowercase())
                } else {
                    line.contains(pattern)
                };
                if matches {
                    out.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
                }
            }
        }
    }
}
