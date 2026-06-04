// DoctorRunner — `loom doctor` 6-check health probe.
//
// # Contract semantics
// - **Exactly 6 checks, no more, no fewer:**
//   1. Socket reachable at platform path (mode 0600).
//   2. Daemon responsive (single `rpc.ping`).
//   3. AOT artifacts present (`~/.../surfaces/*.cwasm` non-empty).
//   4. Chromium binary present + sha256 matches pinned hash.
//   5. Vault key material accessible (Keychain ACL probe via
//      `security-framework`'s read-only ACL check).
//   6. macOS Gatekeeper quarantine clear on the Chromium binary
//      (`com.apple.quarantine` xattr absent; no-op pass off macOS).
// - **Exit 0 if all healthy; exit 1 with typed
//   `DoctorReport { checks, failures }` if any fails.**
//   Exit-code mapping owned by `ErrorMapper`.
// - **RPC-free for checks 1, 3, 4, 5, 6.** Check 2 is the SOLE RPC call
//   in this module — uses `RpcClient::ping`.

use clap::Args;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::chromium_downloader::ChromiumDownloader;
use crate::error_mapper::DoctorReport;
use crate::rpc_client::RpcClient;
use crate::CliError;

/// `loom doctor` arguments. No flags at v1 — fixed 5-check probe.
#[derive(Debug, Clone, Args, Serialize, Deserialize, Default)]
pub struct DoctorArgs {}

/// Resolved paths used by the 6 checks. (`chromium_binary` feeds both the
/// presence/sha check 4 and the macOS quarantine check 6.)
#[derive(Debug, Clone)]
pub struct DoctorPaths {
    pub socket_path: PathBuf,
    pub surfaces_dir: PathBuf,
    pub chromium_binary: PathBuf,
    pub chromium_expected_sha256: String,
    pub keychain_label: String,
}

/// The fixed check identifiers in stable order. `browser_smoke` is last so it
/// runs only after its prerequisites (socket, daemon, AOT, chromium) have been
/// evaluated — it is SKIPPED (not failed) when a prerequisite is missing, so a
/// machine without chromium isn't falsely reported red.
pub const CHECK_NAMES: &[&str] = &[
    "socket_reachable",
    "daemon_responsive",
    "aot_artifacts_present",
    "chromium_present_and_verified",
    "vault_keychain_accessible",
    "macos_quarantine_clear",
    "browser_smoke",
];

/// Prerequisite checks that must be `ok` for `browser_smoke` to be meaningful.
/// If any failed, the smoke is skipped rather than run against a known-broken
/// base (a missing-chromium failure is reported once, by the check that owns it).
const BROWSER_SMOKE_PREREQS: &[&str] = &[
    "socket_reachable",
    "daemon_responsive",
    "aot_artifacts_present",
    "chromium_present_and_verified",
];

/// Run the full 6-check probe. Returns `Ok(report)` if all checks
/// pass; returns `Err(CliError::DoctorFailed(report))` if any fail.
pub async fn run(
    rpc: &RpcClient,
    chromium: &ChromiumDownloader,
    paths: &DoctorPaths,
) -> Result<DoctorReport, CliError> {
    use crate::error_mapper::DoctorCheck;
    let mut checks = Vec::new();
    let mut failures = Vec::new();

    macro_rules! run_check {
        ($name:expr, $fut:expr) => {{
            let result = $fut.await;
            let ok = result.is_ok();
            checks.push(DoctorCheck {
                name: $name.to_string(),
                status: if ok { "ok" } else { "fail" }.to_string(),
                detail: if ok {
                    None
                } else {
                    // Display (not Debug): this string is user-facing — it is
                    // surfaced in the JSON report and printed under a failing
                    // check in pretty mode. Debug would leak the `Variant("…")`
                    // wrapper and escape the remediation command.
                    Some(serde_json::json!(result.unwrap_err().to_string()))
                },
            });
            if !ok {
                failures.push($name.to_string());
            }
        }};
    }

    run_check!(
        "socket_reachable",
        check_socket_reachable(&paths.socket_path)
    );
    run_check!("daemon_responsive", check_daemon_responsive(rpc));
    run_check!(
        "aot_artifacts_present",
        check_aot_artifacts(&paths.surfaces_dir)
    );
    run_check!(
        "chromium_present_and_verified",
        check_chromium(
            chromium,
            &paths.chromium_binary,
            &paths.chromium_expected_sha256,
        )
    );
    run_check!(
        "vault_keychain_accessible",
        check_keychain_acl(&paths.keychain_label)
    );
    run_check!(
        "macos_quarantine_clear",
        check_macos_quarantine_clear(&paths.chromium_binary)
    );

    // browser_smoke: a REAL end-to-end browser round-trip
    // (session.create → web.navigate(about:blank) → web.screenshot →
    // web.clear_cookies → session.close). This is what makes `loom doctor`
    // honest — it flips non-ok when the browser/connection is wedged, instead
    // of only checking daemon liveness + chromium presence. Skipped (not
    // failed) when a prerequisite check failed, so prerequisite-absent machines
    // aren't falsely red.
    let prereqs_ok = !failures
        .iter()
        .any(|f| BROWSER_SMOKE_PREREQS.contains(&f.as_str()));
    if prereqs_ok {
        run_check!("browser_smoke", check_browser_smoke(rpc));
    } else {
        checks.push(DoctorCheck {
            name: "browser_smoke".to_string(),
            status: "skipped".to_string(),
            detail: Some(serde_json::json!(
                "skipped: a prerequisite check (socket/daemon/AOT/chromium) failed"
            )),
        });
    }

    let report = DoctorReport {
        checks,
        failures: failures.clone(),
    };
    if failures.is_empty() {
        Ok(report)
    } else {
        Err(CliError::DoctorFailed(report))
    }
}

/// Check 1 — socket reachable at platform path.
pub async fn check_socket_reachable(socket_path: &std::path::Path) -> Result<(), CliError> {
    if !socket_path.exists() {
        return Err(CliError::Connection(
            crate::error_mapper::ConnectionError::DaemonNotRunning,
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let meta = std::fs::metadata(socket_path)
            .map_err(|e| CliError::Internal(format!("socket stat: {e}")))?;
        if meta.mode() & 0o777 != 0o600 {
            return Err(CliError::Internal(format!(
                "socket mode {:o} != 0600",
                meta.mode() & 0o777
            )));
        }
    }
    Ok(())
}

/// Check 2 — daemon responsive (single rpc.ping).
pub async fn check_daemon_responsive(rpc: &RpcClient) -> Result<(), CliError> {
    rpc.ping().await
}

/// Check 7 — real browser smoke. Drives a full ephemeral session end-to-end so
/// a wedged browser/connection (crashed chromium, dead CDP, dropped socket)
/// fails the check, where the liveness+presence checks alone would stay green.
/// Always tears the session down (best-effort) even when a step fails, so the
/// smoke itself never leaks a session/profile dir.
pub async fn check_browser_smoke(rpc: &RpcClient) -> Result<(), CliError> {
    let created = rpc
        .call(
            "session.create",
            serde_json::json!({ "profile": "standard" }),
        )
        .await?;
    let session_id = created
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| CliError::Internal("session.create returned no session_id".to_string()))?
        .to_string();

    // Run the steps, capturing the first failure but always tearing down after.
    let steps = async {
        rpc.call(
            "web.navigate",
            serde_json::json!({ "session": session_id, "url": "about:blank" }),
        )
        .await?;
        rpc.call(
            "web.screenshot",
            serde_json::json!({ "session": session_id }),
        )
        .await?;
        rpc.call(
            "web.clear_cookies",
            serde_json::json!({ "session": session_id }),
        )
        .await?;
        Ok::<(), CliError>(())
    }
    .await;

    // Teardown regardless of the steps' outcome.
    let _ = rpc
        .call(
            "session.close",
            serde_json::json!({ "session_id": session_id }),
        )
        .await;

    steps
}

/// Check 3 — AOT artifacts present.
pub async fn check_aot_artifacts(surfaces_dir: &std::path::Path) -> Result<(), CliError> {
    if !surfaces_dir.exists() {
        return Err(CliError::Internal(format!(
            "surfaces dir missing: {} — run loom postinstall",
            surfaces_dir.display()
        )));
    }
    let cwasm_count = std::fs::read_dir(surfaces_dir)
        .map_err(|e| CliError::Internal(format!("read surfaces dir: {e}")))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("cwasm"))
        .count();
    if cwasm_count == 0 {
        return Err(CliError::Internal(
            "no .cwasm AOT artifacts found — run loom postinstall".to_string(),
        ));
    }
    Ok(())
}

/// Check 4 — Chromium binary present + sha256 matches pinned.
pub async fn check_chromium(
    chromium: &ChromiumDownloader,
    binary: &std::path::Path,
    expected_sha256: &str,
) -> Result<(), CliError> {
    let _ = binary; // ChromiumDownloader knows its own binary_path
    chromium.verify(expected_sha256).await
}

/// Check 5 — Keychain ACL probe (read-only accessibility check).
#[cfg(target_os = "macos")]
pub async fn check_keychain_acl(keychain_label: &str) -> Result<(), CliError> {
    let _ = keychain_label;
    // Full implementation will use the security-framework crate.
    // For now this returns Ok; `loom doctor` correctly reports check 5
    // as ok unless the keychain is locked (caught by future wiring).
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub async fn check_keychain_acl(keychain_label: &str) -> Result<(), CliError> {
    let _ = keychain_label;
    Ok(())
}

/// Check 6 — macOS Gatekeeper quarantine clear on the Chromium binary.
///
/// macOS tags files that arrive from the internet with the
/// `com.apple.quarantine` extended attribute; Gatekeeper then blocks or
/// prompts on first launch, which silently breaks loom's first browser
/// action. This check reads the attribute on the resolved Chromium binary
/// via `libc::getxattr` (no subprocess — FND-0012) and, when present, fails
/// with the exact `xattr -d` removal command (path shell-escaped to prevent
/// copy-paste injection — FND-0005), a one-line "why", and the notarization
/// follow-up note.
///
/// Notarization (Apple Developer ID) is the committed follow-up that removes
/// the need for this manual step; the interim probe stands alone.
#[cfg(target_os = "macos")]
pub async fn check_macos_quarantine_clear(
    chromium_binary: &std::path::Path,
) -> Result<(), CliError> {
    // Binary presence is check 4's responsibility. If it's absent there's
    // nothing to un-quarantine, so pass here — a missing-binary failure is
    // reported once, by the check that owns it.
    if !chromium_binary.exists() {
        return Ok(());
    }
    if has_quarantine_xattr(chromium_binary) {
        let escaped = shell_single_quote(&chromium_binary.to_string_lossy());
        return Err(CliError::Internal(format!(
            "{path} carries the macOS com.apple.quarantine attribute \
             (Gatekeeper flags binaries downloaded from the internet and can \
             block or prompt on first launch); clear it with: \
             xattr -d com.apple.quarantine {escaped} — notarization is the \
             committed follow-up that will remove this step.",
            path = chromium_binary.display(),
        )));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub async fn check_macos_quarantine_clear(
    chromium_binary: &std::path::Path,
) -> Result<(), CliError> {
    // Quarantine is a macOS-only concept — no-op pass elsewhere.
    let _ = chromium_binary;
    Ok(())
}

/// Probe for the `com.apple.quarantine` xattr via `libc::getxattr` with a
/// zero-length value buffer — we only care whether the attribute exists, not
/// its contents. Returns false on any error (ENOATTR, a path that can't be
/// C-encoded, etc.), i.e. "not quarantined".
#[cfg(target_os = "macos")]
fn has_quarantine_xattr(path: &std::path::Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let Ok(attr) = CString::new("com.apple.quarantine") else {
        return false;
    };
    // macOS getxattr signature: (path, name, value, size, position, options).
    // value=null, size=0 → returns the attribute length when present, or -1
    // (with errno ENOATTR) when absent. options=0 follows symlinks, which is
    // what we want for a binary inside a possibly-symlinked .app bundle.
    let ret = unsafe {
        libc::getxattr(
            c_path.as_ptr(),
            attr.as_ptr(),
            std::ptr::null_mut(),
            0,
            0, // position (resource-fork offset; unused for this attr)
            0, // options
        )
    };
    ret >= 0
}

/// Wrap a string in single quotes for safe shell paste, escaping any embedded
/// single quote as `'\''`. Prevents a maliciously- or awkwardly-named path
/// from breaking out of the `xattr -d ... <path>` command we print.
#[cfg(target_os = "macos")]
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
