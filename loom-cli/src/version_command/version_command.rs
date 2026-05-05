// VersionCommand — `loom --version` handler.
//
// # Contract semantics
// - **SR-CLI-01 / AC-NFR-PERF.** Bypasses `RpcClient` entirely.
//   Resolves `env!("CARGO_PKG_VERSION")` + compile-time provenance
//   constants set by `build.rs`. No socket I/O. No schema load.
//   p99 < 50 ms target, contract SLA p99 ≤ 200 ms.
// - **Output.** Emits a single canonical-JSON object via
//   `OutputFormatter`; payload is `VersionInfo`.

use serde::{Deserialize, Serialize};

use crate::CliError;

/// Display string used by clap's `--version` (AC-VER-02). Format:
/// `<semver> (<short-sha> <build-date>)`. clap prepends the binary
/// name, so `loom --version` prints `loom 0.9.0 (abc1234 2026-05-04)`.
pub const LOOM_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("LOOM_GIT_SHA"),
    " ",
    env!("LOOM_BUILD_DATE"),
    ")"
);

/// Compile-time version data. All fields are filled by build-time
/// constants — no runtime resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    /// `env!("CARGO_PKG_VERSION")` at build time.
    pub version: String,
    /// `env!("LOOM_GIT_SHA")` at build time (set by `build.rs`; falls
    /// back to `"unknown"` when `.git` is absent, e.g. tarball builds).
    pub git_sha: String,
    /// `env!("LOOM_BUILD_DATE")` at build time, YYYY-MM-DD in UTC. Set
    /// by `build.rs`; honors `SOURCE_DATE_EPOCH` for reproducible builds.
    pub build_date: String,
    /// `env!("TARGET")` triple (e.g. `aarch64-apple-darwin`).
    pub target: String,
}

/// Resolve `VersionInfo` from compile-time constants. Pure; cannot
/// fail.
pub fn resolve() -> VersionInfo {
    VersionInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        git_sha: env!("LOOM_GIT_SHA").to_string(),
        build_date: env!("LOOM_BUILD_DATE").to_string(),
        // std::env::consts provides the target arch/os at runtime (always available).
        target: format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
    }
}

/// `loom --version` entrypoint. Writes one canonical-JSON object to
/// stdout via the default `OutputFormatter` path. Bypasses `RpcClient`.
pub fn print() -> Result<(), CliError> {
    let info = resolve();
    let json =
        serde_json::to_string(&info).map_err(|e| CliError::Internal(e.to_string()))?;
    println!("{json}");
    Ok(())
}
