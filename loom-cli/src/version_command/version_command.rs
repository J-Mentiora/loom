// Re-export of the locked Phase 5.3 interface. DO NOT EDIT here.
// Edit `systems/loom-cli/modules/VersionCommand/interfaces.rs` instead.
// VersionCommand — `loom --version` handler.
//
// # Contract semantics
// - **SR-CLI-01 / AC-NFR-PERF.** Bypasses `RpcClient` entirely.
//   Resolves `env!("CARGO_PKG_VERSION")` + compile-time `git-sha`
//   constant. No socket I/O. No schema load. p99 < 50 ms target,
//   contract SLA p99 ≤ 200 ms.
// - **Output.** Emits a single canonical-JSON object via
//   `OutputFormatter`; payload is `VersionInfo`.

use serde::{Deserialize, Serialize};

use crate::CliError;

/// Compile-time version data. All fields are filled by build-time
/// constants — no runtime resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    /// `env!("CARGO_PKG_VERSION")` at build time.
    pub version: String,
    /// `env!("LOOM_GIT_SHA")` at build time (set by `build.rs`).
    pub git_sha: String,
    /// `env!("TARGET")` triple (e.g. `aarch64-apple-darwin`).
    pub target: String,
}

/// Resolve `VersionInfo` from compile-time constants. Pure; cannot
/// fail.
pub fn resolve() -> VersionInfo {
    VersionInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        git_sha: option_env!("LOOM_GIT_SHA").unwrap_or("unknown").to_string(),
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
