//! `wasm_host` — see `systems/loom-host/modules/wasm_host/interfaces.rs`
//! for the locked Phase 5.3 interface. Re-exports it verbatim via
//! `include!`, keeping `systems/` the single source of truth.
pub mod wasm_host;
pub use wasm_host::*;

use loom_shared::error_format::{LoomError, LoomErrorCode};

/// Check that the host OS version meets the minimum requirement
/// (macOS 14+ for the v1 release). Pass `version_override` in tests
/// to avoid spawning `sw_vers`; `WasmHost::new` reads
/// `LOOM_OS_VERSION_OVERRIDE` and forwards it here.
///
/// When `version_override` is `Some(s)`, the version string is used
/// on all platforms (so tests can exercise the check on Linux too).
/// When `None` and on macOS, `sw_vers -productVersion` is spawned.
/// When `None` and not on macOS, the check passes unconditionally.
//
// `manual_map` here only fires under `cfg(not(target_os = "macos"))`, where the
// else branch reduces to `None`. On macOS the else runs `sw_vers`, so the
// suggested `version_override.map(...)` rewrite would be wrong. Suppress.
#[allow(clippy::manual_map)]
pub(crate) fn check_platform_version(version_override: Option<&str>) -> Result<(), LoomError> {
    use loom_shared::error_format::LoomErrorCode;

    let version_str: Option<String> = if let Some(v) = version_override {
        Some(v.to_owned())
    } else {
        #[cfg(target_os = "macos")]
        {
            let out = std::process::Command::new("sw_vers")
                .arg("-productVersion")
                .output()
                .map_err(|e| LoomError::new(LoomErrorCode::Internal, e.to_string()))?;
            Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    };

    if let Some(ref ver) = version_str {
        let major: u32 = ver
            .split('.')
            .next()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        if major < 14 {
            return Err(LoomError::new(
                LoomErrorCode::Unsupported,
                "platform_unsupported: loom requires macOS 14 (Sonoma) or later".to_owned(),
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod interface_tests;

#[cfg(test)]
mod tests;
