// LoomBinariesDownloader — download + sha256-verify the auxiliary loom
// binaries (`loom-daemon`, `loom-mcp`, `loom-shim-chromium`) from the GH
// Release matching the installed `loom-cli` crate version.
//
// Why this exists: `cargo install --git URL --tag v{X}.{Y}.{Z} loom-cli`
// produces only the `loom` binary on the user's PATH. The runtime spawns
// 3 sibling binaries; brew + manual download bundle all 4 in the
// cargo-dist tarball, but the cargo-install path is single-binary.
// Postinstall calls `ensure(...)` to fetch the missing 3 from the same
// release, verifying SHA-256 against the release's `dist-manifest.json`.
//
// # Contract semantics
// - **AC-DIST-01.** Single-binary cargo-install gets the 3 siblings via
//   postinstall — same final state as brew/manual.
// - **SHA verify.** Every downloaded tarball is recomputed and compared
//   against `artifact.checksums["sha256"]` from the release manifest.
//   Mismatch → `CliError::SupplyChain` (same variant `chromium_downloader`
//   uses, mapped to `LoomErrorCode::SupplyChainViolation`).
// - **Idempotent.** Sentinel file `install_dir/.loom_binaries_sha256`
//   stores the manifest tarball SHA. Re-run with matching sentinel skips.
// - **Version-pinned.** Caller passes `version` (typically
//   `env!("CARGO_PKG_VERSION")`); we GET
//   `https://github.com/J-Mentiora/loom/releases/download/v{version}/dist-manifest.json`.
//   404 → typed `CliError::Internal` with the actionable correction
//   ("install with --tag v{X}.{Y}.{Z}").
// - **Internal-only.** Used by `PostinstallRunner::loom_binaries_step`.

use std::path::{Path, PathBuf};

use crate::CliError;

/// Repository owner/name segment for the GH Release URL. Match the
/// workspace `repository` metadata in Cargo.toml.
const RELEASE_OWNER_REPO: &str = "J-Mentiora/loom";

/// The 3 sibling binaries cargo-install must acquire post-fact.
/// (The `loom` binary itself was installed by `cargo install` already.)
pub const AUX_BINARY_NAMES: &[&str] = &["loom-daemon", "loom-mcp", "loom-shim-chromium"];

/// Outcome of an `ensure` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadOutcome {
    /// All 3 binaries were already present and the sentinel matched
    /// the manifest SHA.
    Skipped,
    /// Tarball was fetched + verified; binaries extracted to install_dir.
    Downloaded,
}

/// Default install directory: `dirs::data_local_dir()/loom/bin/`.
/// `~/Library/Application Support/loom/bin/` on macOS;
/// `~/.local/share/loom/bin/` on Linux.
pub fn default_install_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("loom").join("bin"))
}

/// Ensure the 3 auxiliary binaries are present at `install_dir` and match
/// the sha256 published in the release's `dist-manifest.json`.
///
/// # Arguments
/// - `version`: workspace crate version (e.g. `"1.0.0"`); the release tag
///   is `v{version}`.
/// - `target_triple`: rustc-style host triple (e.g. `"aarch64-apple-darwin"`).
///   Use [`host_target_triple()`] in the common case.
/// - `install_dir`: where to drop the 3 binaries (typically
///   [`default_install_dir()`]).
pub async fn ensure(
    version: &str,
    target_triple: &str,
    install_dir: &Path,
) -> Result<DownloadOutcome, CliError> {
    // Fetch manifest first — this also tells us the artifact name + SHA.
    let manifest_url = format!(
        "https://github.com/{}/releases/download/v{}/dist-manifest.json",
        RELEASE_OWNER_REPO, version
    );
    let manifest_bytes = curl_get(&manifest_url).map_err(|e| {
        // Distinguish 404 (release doesn't exist) from generic curl failure.
        if e.is_404 {
            CliError::Internal(format!(
                "no release artifact for v{version}; \
                 install with `cargo install --git https://github.com/{RELEASE_OWNER_REPO} \
                 --tag v<existing-release-version> loom-cli`"
            ))
        } else {
            CliError::Internal(format!("fetch manifest from {manifest_url}: {}", e.message))
        }
    })?;
    let manifest: DistManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| CliError::Internal(format!("parse dist-manifest.json: {e}")))?;

    // Find the tarball artifact for our host triple.
    let artifact = manifest
        .artifacts
        .into_values()
        .find(|a| {
            a.target_triples.iter().any(|t| t == target_triple)
                && a.name
                    .as_ref()
                    .is_some_and(|n| n.ends_with(".tar.xz") || n.ends_with(".tar.gz"))
        })
        .ok_or_else(|| {
            CliError::Internal(format!(
                "dist-manifest for v{version} contains no tarball artifact for {target_triple}"
            ))
        })?;

    let tarball_name = artifact.name.expect("filtered for Some(name) above");
    let expected_sha = artifact
        .checksums
        .get("sha256")
        .ok_or_else(|| {
            CliError::Internal(format!(
                "dist-manifest artifact {tarball_name} has no sha256 checksum"
            ))
        })?
        .to_lowercase();

    // Idempotence sentinel: same SHA + all 3 binaries present → Skipped.
    let sentinel = sentinel_path(install_dir);
    if sentinel.exists() && all_aux_binaries_present(install_dir) {
        let recorded = std::fs::read_to_string(&sentinel)
            .map_err(|e| CliError::Internal(format!("read sentinel: {e}")))?;
        if recorded.trim() == expected_sha {
            return Ok(DownloadOutcome::Skipped);
        }
    }

    std::fs::create_dir_all(install_dir)
        .map_err(|e| CliError::Internal(format!("create install_dir: {e}")))?;

    // Download the tarball to a temp file alongside the install dir.
    let tarball_url = format!(
        "https://github.com/{}/releases/download/v{}/{}",
        RELEASE_OWNER_REPO, version, tarball_name
    );
    let tmp_tarball = install_dir.join(format!(".{}.download.tmp", tarball_name));
    let status = std::process::Command::new("curl")
        .args(["-fsSL", "-L", "-o"])
        .arg(&tmp_tarball)
        .arg(&tarball_url)
        .status()
        .map_err(|e| CliError::Internal(format!("curl: {e}")))?;
    if !status.success() {
        return Err(CliError::Internal(format!(
            "curl download failed for {tarball_url}"
        )));
    }

    // SHA verify before extraction.
    let actual = crate::chromium_downloader::sha256_of_file(&tmp_tarball)
        .await?
        .to_lowercase();
    if actual != expected_sha {
        let _ = std::fs::remove_file(&tmp_tarball);
        return Err(CliError::SupplyChain {
            expected_hash: expected_sha,
            actual_hash: actual,
            url: tarball_url,
        });
    }

    // Extract via system `tar` — handles both .tar.xz and .tar.gz uniformly.
    let extract_dir = install_dir.join(".extract.tmp");
    let _ = std::fs::remove_dir_all(&extract_dir);
    std::fs::create_dir_all(&extract_dir)
        .map_err(|e| CliError::Internal(format!("create extract dir: {e}")))?;
    let tar_status = std::process::Command::new("tar")
        .arg("-xf")
        .arg(&tmp_tarball)
        .arg("-C")
        .arg(&extract_dir)
        .status()
        .map_err(|e| CliError::Internal(format!("tar spawn: {e}")))?;
    if !tar_status.success() {
        let _ = std::fs::remove_dir_all(&extract_dir);
        let _ = std::fs::remove_file(&tmp_tarball);
        return Err(CliError::Internal(format!(
            "tar extract failed for {}",
            tmp_tarball.display()
        )));
    }
    let _ = std::fs::remove_file(&tmp_tarball);

    // cargo-dist tarballs have a single top-level directory named like the
    // artifact stem (e.g. `loom-aarch64-apple-darwin/`). Walk one level deep
    // to locate the 3 binaries. Tolerate either layout (top-level or nested).
    for binary in AUX_BINARY_NAMES {
        let src = find_extracted_binary(&extract_dir, binary).ok_or_else(|| {
            CliError::Internal(format!(
                "tarball {tarball_name} did not contain {binary} at any depth"
            ))
        })?;
        let dst = install_dir.join(binary);
        // Replace any prior copy.
        let _ = std::fs::remove_file(&dst);
        std::fs::copy(&src, &dst).map_err(|e| CliError::Internal(format!("copy {binary}: {e}")))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| CliError::Internal(format!("chmod {binary}: {e}")))?;
        }
    }

    let _ = std::fs::remove_dir_all(&extract_dir);

    // Sentinel last (matches chromium_downloader semantics).
    std::fs::write(&sentinel, &expected_sha)
        .map_err(|e| CliError::Internal(format!("write sentinel: {e}")))?;

    Ok(DownloadOutcome::Downloaded)
}

/// Returns the rustc-style host triple loom-cli was compiled for.
/// Falls back to a generic identifier on unsupported targets so the manifest
/// lookup yields a clear "no artifact for <X>" error rather than a panic.
pub fn host_target_triple() -> &'static str {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "aarch64-apple-darwin"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "x86_64-apple-darwin"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
    {
        "x86_64-unknown-linux-gnu"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64", target_env = "gnu"))]
    {
        "aarch64-unknown-linux-gnu"
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"),
        all(target_os = "linux", target_arch = "aarch64", target_env = "gnu"),
    )))]
    {
        "unsupported-target"
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn sentinel_path(install_dir: &Path) -> PathBuf {
    install_dir.join(".loom_binaries_sha256")
}

fn all_aux_binaries_present(install_dir: &Path) -> bool {
    AUX_BINARY_NAMES
        .iter()
        .all(|n| install_dir.join(n).is_file())
}

/// Walk `dir` (max depth 3) looking for an executable file whose filename
/// matches `name`. cargo-dist tarballs typically nest binaries one level
/// deep under a `<artifact-stem>/` directory.
fn find_extracted_binary(dir: &Path, name: &str) -> Option<PathBuf> {
    fn walk(dir: &Path, name: &str, depth: usize) -> Option<PathBuf> {
        if depth == 0 {
            return None;
        }
        let entries = std::fs::read_dir(dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.file_name().and_then(|s| s.to_str()) == Some(name) {
                return Some(path);
            }
            if path.is_dir() {
                if let Some(p) = walk(&path, name, depth - 1) {
                    return Some(p);
                }
            }
        }
        None
    }
    walk(dir, name, 3)
}

/// Minimal HTTP GET via the system `curl` subprocess. Returns the raw body
/// bytes. Distinguishes 404 from other errors so the caller can surface a
/// version-skew-specific message.
struct CurlError {
    is_404: bool,
    message: String,
}

fn curl_get(url: &str) -> Result<Vec<u8>, CurlError> {
    // -w '%{http_code}' on stderr is hard to disentangle from the body.
    // Easier: use `--fail-with-body` (curl 7.76+) to fail with non-zero on
    // 4xx/5xx but still print the body. Fall back to plain --fail otherwise.
    let output = std::process::Command::new("curl")
        .args(["-sSL", "-o", "-"])
        .arg("-w")
        .arg("\n%{http_code}")
        .arg(url)
        .output()
        .map_err(|e| CurlError {
            is_404: false,
            message: format!("curl spawn: {e}"),
        })?;
    if !output.status.success() {
        return Err(CurlError {
            is_404: false,
            message: format!(
                "curl exit {}: {}",
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    // Tail of stdout is the http code (after final newline).
    let stdout = output.stdout;
    let split_at = stdout
        .iter()
        .rposition(|&b| b == b'\n')
        .ok_or_else(|| CurlError {
            is_404: false,
            message: "curl output had no http code suffix".to_string(),
        })?;
    let (body, code_chunk) = stdout.split_at(split_at);
    let code = String::from_utf8_lossy(&code_chunk[1..]).trim().to_string();
    if code == "404" {
        return Err(CurlError {
            is_404: true,
            message: format!("404 Not Found at {url}"),
        });
    }
    if !code.starts_with('2') {
        return Err(CurlError {
            is_404: false,
            message: format!("HTTP {code} from {url}"),
        });
    }
    Ok(body.to_vec())
}

// ---------------------------------------------------------------------------
// dist-manifest.json shadow types — only the fields we need.
// Full schema lives in `cargo-dist-schema` crate; pulling it would balloon
// the dependency tree for 3 fields.
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct DistManifest {
    #[serde(default)]
    artifacts: std::collections::BTreeMap<String, Artifact>,
}

#[derive(serde::Deserialize)]
struct Artifact {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    target_triples: Vec<String>,
    #[serde(default)]
    checksums: std::collections::BTreeMap<String, String>,
}
