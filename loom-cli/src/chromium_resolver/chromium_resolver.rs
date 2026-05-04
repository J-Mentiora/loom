// chromium_resolver — locate a Chromium binary across install channels.
//
// AC-DIST-05. Resolution order (top wins):
//   1. `LOOM_CHROMIUM_PATH` env (explicit user override) → ChromiumSource::EnvOverride
//   2. `<chromium_dir>/Chromium.app/Contents/MacOS/Chromium`
//      (the path `loom postinstall` writes to) → ChromiumSource::Pinned
//   3. Existing AC-CHBS-01 parent-dir scan: any executable inside
//      `<chromium_dir>/Chromium.app/Contents/MacOS/` (covers symlink-renamed
//      bundles like `Google Chrome`) → ChromiumSource::Pinned
//   4. PATH lookup for `chromium`, `chromium-browser`, `chrome`,
//      `google-chrome` → ChromiumSource::Path
//   5. macOS standard installs:
//      - `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`
//      - `/Applications/Chromium.app/Contents/MacOS/Chromium`
//      → ChromiumSource::Applications
//   6. None → `Err(BrowserNotFound { searched_paths })`
//
// All internal errors (`std::io::Error` from `metadata()`, `var_os()`,
// broken symlinks, permission denied) are mapped to `BrowserNotFound` so
// the resolver's signature stays `Result<(PathBuf, ChromiumSource), BrowserNotFound>`
// — never panics, never propagates io::Error.
//
// Linux wrapper scripts (e.g. `/usr/bin/google-chrome` on Debian/Ubuntu is
// a shell wrapper that re-execs the real binary) are accepted verbatim:
// `tokio::process::Command::new` passes args through and CDP attaches over
// `--remote-debugging-port` exactly as if we'd called the real binary.
//
// SECURITY: PATH search trusts the caller's PATH ordering. A PATH entry
// with `.` or a compromised dir could shadow chromium. This is generic to
// PATH-based binary lookup (cargo, git, make, etc.); we don't add explicit
// guards — Loom users who installed via brew or cargo already trust their PATH.

use std::path::{Path, PathBuf};

/// Where the resolved chromium came from. Used by the daemon to emit a
/// `tracing::warn!` when source is not `Pinned` (D10 — preserves the
/// determinism contract's visibility).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromiumSource {
    /// `LOOM_CHROMIUM_PATH` env was set and validated.
    EnvOverride,
    /// `loom postinstall` wrote a pinned Chromium to `<chromium_dir>`.
    /// This is the only source that guarantees replay-bit-equality
    /// across machines (per SR-SHIM-03).
    Pinned,
    /// Resolved via `$PATH` (chromium / chromium-browser / chrome /
    /// google-chrome). Determinism not guaranteed.
    Path,
    /// macOS-standard install location (`/Applications/...`). Determinism
    /// not guaranteed.
    Applications,
}

/// Resolution failure. Carries the full search-path list for diagnostics
/// and the platform-aware install hint (filled in by `Display` in the CLI).
#[derive(Debug, Clone)]
pub struct BrowserNotFound {
    pub searched_paths: Vec<String>,
}

impl std::fmt::Display for BrowserNotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Chromium not found. Searched: {}",
            self.searched_paths.join(", ")
        )
    }
}

impl std::error::Error for BrowserNotFound {}

const PATH_NAMES: &[&str] = &["chromium", "chromium-browser", "chrome", "google-chrome"];

#[cfg(target_os = "macos")]
const APPLICATIONS_PATHS: &[&str] = &[
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
];

#[cfg(not(target_os = "macos"))]
const APPLICATIONS_PATHS: &[&str] = &[];

/// Resolve a Chromium binary for the host process. The `chromium_dir` is
/// the same `~/.config/loom/chromium/` path used by `loom postinstall`,
/// `loom doctor`, and the daemon's pre-existing hardcode.
pub fn resolve_chromium(chromium_dir: &Path) -> Result<(PathBuf, ChromiumSource), BrowserNotFound> {
    let mut searched: Vec<String> = Vec::new();

    // 1. LOOM_CHROMIUM_PATH explicit override.
    if let Some(env_val) = std::env::var_os("LOOM_CHROMIUM_PATH") {
        let p = PathBuf::from(&env_val);
        searched.push(format!("LOOM_CHROMIUM_PATH={}", p.display()));
        if is_valid_executable(&p) {
            return Ok((p, ChromiumSource::EnvOverride));
        }
        // Per D11: don't fatally error on a stale env var. Log + fall through.
        tracing::warn!(
            "LOOM_CHROMIUM_PATH={} is not an executable file; falling through to standard resolution",
            p.display()
        );
    }

    // 2. Pinned: `<chromium_dir>/Chromium.app/Contents/MacOS/Chromium`.
    let pinned = chromium_dir.join("Chromium.app/Contents/MacOS/Chromium");
    searched.push(pinned.display().to_string());
    if is_valid_executable(&pinned) {
        return Ok((pinned, ChromiumSource::Pinned));
    }

    // 3. AC-CHBS-01 parent-dir scan: any executable in MacOS/ (handles
    // bundles where the inner binary is named `Google Chrome` instead of
    // `Chromium` — symlinks/renamed casks).
    if let Some(parent) = pinned.parent() {
        if parent.is_dir() {
            if let Some(found) = first_executable_in_dir(parent) {
                searched.push(format!("{}/<scan>", parent.display()));
                return Ok((found, ChromiumSource::Pinned));
            }
        }
    }

    // 4. PATH search.
    for name in PATH_NAMES {
        if let Some(found) = lookup_in_path(name) {
            searched.push(format!("PATH:{name}"));
            return Ok((found, ChromiumSource::Path));
        }
        searched.push(format!("PATH:{name}"));
    }

    // 5. macOS Applications fallbacks.
    for app_path in APPLICATIONS_PATHS {
        let p = PathBuf::from(app_path);
        searched.push(app_path.to_string());
        if is_valid_executable(&p) {
            return Ok((p, ChromiumSource::Applications));
        }
    }

    Err(BrowserNotFound {
        searched_paths: searched,
    })
}

/// True iff `path` resolves to a regular file (following symlinks) with
/// at least one execute bit set on Unix. Returns false on any io::Error
/// (permission denied, broken symlink, missing). Non-Unix returns false —
/// we don't ship Windows.
fn is_valid_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        // metadata() follows symlinks → broken symlinks → io::Error → false.
        let Ok(meta) = std::fs::metadata(path) else {
            return false;
        };
        meta.is_file() && (meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

/// Scan `dir` for the first executable regular file. Mirrors the existing
/// `find_executable_in_dir` in `chromium_downloader.rs` so the AC-CHBS-01
/// behavior stays identical. Returns `None` on any io::Error.
fn first_executable_in_dir(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.filter_map(|e| e.ok()) {
        let p = entry.path();
        if is_valid_executable(&p) {
            return Some(p);
        }
    }
    None
}

/// Look up `name` in `$PATH`. Returns the first hit that is a regular
/// executable file. None if PATH is unset or no entry matches.
fn lookup_in_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if is_valid_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}
