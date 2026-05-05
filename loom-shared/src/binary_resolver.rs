//! `loom-shared::binary_resolver` — locate sibling loom binaries on disk.
//!
//! `loom`, `loom-daemon`, `loom-mcp`, `loom-shim-chromium` are co-located
//! in install paths produced by Homebrew + manual tarball downloads
//! (cargo-dist bundles all 4 in the same tarball). For the `cargo install`
//! path only `loom` lives in `~/.cargo/bin/`; the other three are dropped
//! by `loom postinstall` into `dirs::data_local_dir().join("loom/bin/")`.
//!
//! Resolution order (first hit wins):
//! 1. `dirs::data_local_dir()/loom/bin/<name>` — postinstall-managed location
//!    (cargo-install path).
//! 2. `current_exe().parent()/<name>` — co-located install (brew / manual).
//! 3. PATH lookup via `which::which` — fallback for unusual setups (PATH
//!    edits, symlinks, container layouts).
//!
//! Returns `None` only when all three lookups miss; callers decide whether
//! that is a typed error (daemon spawn) or a soft warning (doctor).

use std::path::PathBuf;

/// Find a loom sibling binary by name (e.g. `"loom-daemon"`,
/// `"loom-shim-chromium"`).
///
/// Resolution order documented at the module level. Returns `None` if no
/// candidate path exists.
pub fn resolve_loom_sibling(name: &str) -> Option<PathBuf> {
    if let Some(p) = data_local_candidate(name) {
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(p) = current_exe_sibling(name) {
        if p.is_file() {
            return Some(p);
        }
    }
    path_lookup(name)
}

/// `dirs::data_local_dir()/loom/bin/<name>`. Returns `None` when the OS has
/// no concept of a local data dir (extremely rare; e.g. `$HOME` unset on
/// non-Windows).
pub fn data_local_candidate(name: &str) -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("loom").join("bin").join(name))
}

/// `current_exe().parent()/<name>`. Returns `None` when `current_exe()`
/// fails (sandbox, bad procfs, etc.) or has no parent (root).
pub fn current_exe_sibling(name: &str) -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.join(name)))
}

/// PATH lookup using a minimal in-tree implementation (no `which` crate
/// dependency). Iterates `$PATH`, returning the first executable named
/// `name` (with the platform's executable extension on Windows).
fn path_lookup(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            for ext in ["exe", "cmd", "bat"] {
                let candidate = dir.join(format!("{}.{}", name, ext));
                if is_executable(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &std::path::Path) -> bool {
    p.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `data_local_candidate` produces a path under the system's data-local
    /// dir, with the expected `loom/bin/<name>` suffix.
    #[test]
    fn data_local_candidate_has_loom_bin_suffix() {
        // dirs::data_local_dir() is platform-defined and present on every
        // supported target (macOS arm64/x86, Linux x86/arm64). It can return
        // None if HOME is unset; in CI test runners HOME is always set.
        let p = data_local_candidate("loom-daemon")
            .expect("data_local_dir should resolve in a test environment");
        let suffix = p.iter().rev().take(3).collect::<Vec<_>>();
        assert_eq!(suffix[0].to_str(), Some("loom-daemon"));
        assert_eq!(suffix[1].to_str(), Some("bin"));
        assert_eq!(suffix[2].to_str(), Some("loom"));
    }

    /// `current_exe_sibling` returns a path with the requested filename.
    #[test]
    fn current_exe_sibling_uses_requested_name() {
        let p = current_exe_sibling("loom-test-name").expect("current_exe() resolves in tests");
        assert_eq!(
            p.file_name().and_then(|s| s.to_str()),
            Some("loom-test-name")
        );
    }

    /// `resolve_loom_sibling` returns None when the binary genuinely doesn't
    /// exist in any of the three lookup locations.
    #[test]
    fn returns_none_when_missing_everywhere() {
        // Pick a name vanishingly unlikely to exist as a real binary.
        let result = resolve_loom_sibling("loom-zzzz9999-nonexistent-binary");
        assert!(
            result.is_none(),
            "expected None for missing binary, got {:?}",
            result
        );
    }
}
