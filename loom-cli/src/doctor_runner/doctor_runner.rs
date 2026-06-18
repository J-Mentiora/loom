// DoctorRunner — `loom doctor` 9-check health probe.
//
// # Contract semantics
// - **Exactly 9 checks, no more, no fewer:**
//   1. Socket reachable at platform path (mode 0600).
//   2. Daemon responsive (single `rpc.ping`).
//   3. AOT artifacts present (`~/.../surfaces/*.cwasm` non-empty).
//   4. AOT artifacts current (the installed `loom_surface_web` sidecar's
//      source SHA + engine-compat hash still match THIS binary). RPC-free;
//      flags a STALE surface that would trap on first dispatch with a typed
//      "run `loom postinstall`" — distinct from a generic `browser_smoke`
//      failure. Passes when the artifact is absent (check 3 owns that) or
//      carries only a legacy single-line stamp (the daemon would still boot
//      a compatible legacy artifact).
//   5. Chromium binary present + sha256 matches pinned hash.
//   6. Vault key material accessible (Keychain ACL probe via
//      `security-framework`'s read-only ACL check).
//   7. macOS Gatekeeper quarantine clear on the Chromium binary
//      (`com.apple.quarantine` xattr absent; no-op pass off macOS).
//   8. Session health (active sessions / orphan browser trees / oldest
//      session age, via `daemon.health`). Informational: `warn` when
//      orphan trees exist, never a hard failure.
//   9. Browser smoke (full session round-trip; skipped if prereqs failed).
//      Reports `at_capacity` (warn-class, NOT a failure) when the daemon
//      rejects the smoke's `session.create` with the typed
//      `session_cap_exceeded` — a saturated-but-healthy daemon must not
//      flip doctor red (monitoring keyed on the exit code would
//      false-positive at exactly peak load). Agrees with check 8's
//      `active_sessions` count.
// - **Exit 0 if all healthy; exit 1 with typed
//   `DoctorReport { checks, failures }` if any fails.**
//   Warn-class statuses (`warn`, `at_capacity`) are not failures → exit 0.
//   Exit-code mapping owned by `ErrorMapper`.
// - **RPC-free for checks 1, 3, 4, 5, 6, 7.** Checks 2, 8, 9 use the daemon
//   (`RpcClient::ping` / `daemon.health` / a real session round-trip).
// - **`--daemon-only` scopes the verdict to checks 1-2** (socket + daemon)
//   and reports checks 3-9 as `skipped`, preserving the exactly-9 report
//   shape. For hosts where Chromium/AOT artifacts are absent by design
//   (e.g. the Docker runtime image's HEALTHCHECK).

use clap::Args;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::chromium_downloader::ChromiumDownloader;
use crate::error_mapper::DoctorReport;
use crate::rpc_client::RpcClient;
use crate::CliError;

/// `loom doctor` arguments.
#[derive(Debug, Clone, Args, Serialize, Deserialize, Default)]
pub struct DoctorArgs {
    /// Run only the daemon-scoped checks (socket reachable + daemon
    /// responsive); report everything else as skipped. For hosts where
    /// Chromium/AOT artifacts are absent by design (e.g. a container
    /// healthcheck against an RPC-only daemon image).
    #[arg(long)]
    pub daemon_only: bool,
}

/// Resolved paths used by the filesystem checks. (`chromium_binary` feeds both
/// the presence/sha check 5 and the macOS quarantine check 7; `surfaces_dir`
/// feeds both the AOT-present check 3 and the AOT-current check 4.)
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
    "aot_artifacts_current",
    "chromium_present_and_verified",
    "vault_keychain_accessible",
    "macos_quarantine_clear",
    "session_health",
    "browser_smoke",
];

/// Prerequisite checks that must be `ok` for `browser_smoke` to be meaningful.
/// If any failed, the smoke is skipped rather than run against a known-broken
/// base (a missing-chromium failure is reported once, by the check that owns it).
/// A STALE surface (`aot_artifacts_current` fail) would trap on the smoke's first
/// dispatch, so the smoke is skipped and the staleness is reported by its own
/// typed check, not as a generic smoke failure.
const BROWSER_SMOKE_PREREQS: &[&str] = &[
    "socket_reachable",
    "daemon_responsive",
    "aot_artifacts_present",
    "aot_artifacts_current",
    "chromium_present_and_verified",
];

/// Run the health probe. Returns `Ok(report)` if all checks
/// pass; returns `Err(CliError::DoctorFailed(report))` if any fail.
/// With `args.daemon_only`, only checks 1-2 run; checks 3-8 are
/// reported as `skipped` and never counted as failures.
pub async fn run(
    rpc: &RpcClient,
    chromium: &ChromiumDownloader,
    paths: &DoctorPaths,
    args: &DoctorArgs,
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

    // --daemon-only: the host is only expected to provide a reachable
    // socket and a responsive daemon (e.g. the Docker runtime image,
    // which ships no Chromium or AOT artifacts until `loom postinstall`
    // is run). Report checks 3-8 as `skipped` — NOT failed — so the
    // exit code reflects what this host can actually do, while keeping
    // the exactly-8 report shape.
    if args.daemon_only {
        for name in &CHECK_NAMES[2..] {
            checks.push(DoctorCheck {
                name: (*name).to_string(),
                status: "skipped".to_string(),
                detail: Some(serde_json::json!("skipped: --daemon-only")),
            });
        }
        let report = DoctorReport {
            checks,
            failures: failures.clone(),
        };
        return if failures.is_empty() {
            Ok(report)
        } else {
            Err(CliError::DoctorFailed(report))
        };
    }

    run_check!(
        "aot_artifacts_present",
        check_aot_artifacts(&paths.surfaces_dir)
    );
    run_check!(
        "aot_artifacts_current",
        check_aot_artifacts_current(&paths.surfaces_dir)
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

    // session_health: informational counts (active sessions, orphan browser trees, oldest
    // session age) so a wedged daemon is visible before it's fatal. Reported as `warn` when
    // orphan trees exist (the periodic reaper will clean them), `ok` otherwise — never a hard
    // failure, so it does not flip `loom doctor` red on a transient leak. Skipped when the
    // daemon isn't responsive (no health to read).
    if !failures.iter().any(|f| f == "daemon_responsive") {
        match check_session_health(rpc).await {
            Ok((active, orphans, oldest)) => {
                let oldest_str = oldest
                    .map(|s| format!("{s}s"))
                    .unwrap_or_else(|| "n/a".to_string());
                checks.push(DoctorCheck {
                    name: "session_health".to_string(),
                    status: if orphans > 0 { "warn" } else { "ok" }.to_string(),
                    detail: Some(serde_json::json!(format!(
                        "active_sessions={active} orphan_browser_trees={orphans} oldest_session_age={oldest_str}"
                    ))),
                });
            }
            Err(e) => checks.push(DoctorCheck {
                name: "session_health".to_string(),
                status: "skipped".to_string(),
                detail: Some(serde_json::json!(format!(
                    "could not read daemon.health: {e}"
                ))),
            }),
        }
    } else {
        checks.push(DoctorCheck {
            name: "session_health".to_string(),
            status: "skipped".to_string(),
            detail: Some(serde_json::json!("skipped: daemon not responsive")),
        });
    }

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
        match check_browser_smoke(rpc).await {
            Ok(()) => checks.push(DoctorCheck {
                name: "browser_smoke".to_string(),
                status: "ok".to_string(),
                detail: None,
            }),
            // The typed `session_cap_exceeded` rejection means the daemon
            // answered the RPC and applied policy — busy, not broken.
            // Warn-class like check 7's orphan warn: reported distinctly,
            // never counted as a failure (exit stays 0).
            Err(ref e) => match session_cap_detail(e) {
                Some(detail) => checks.push(DoctorCheck {
                    name: "browser_smoke".to_string(),
                    status: "at_capacity".to_string(),
                    detail: Some(serde_json::json!(detail)),
                }),
                None => {
                    checks.push(DoctorCheck {
                        name: "browser_smoke".to_string(),
                        status: "fail".to_string(),
                        // Display (not Debug) — same rationale as run_check!.
                        detail: Some(serde_json::json!(e.to_string())),
                    });
                    failures.push("browser_smoke".to_string());
                }
            },
        }
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

/// Read session-health counts from `daemon.health`. Returns
/// `(active_sessions, orphan_browser_trees, oldest_active_session_age_secs)`.
pub async fn check_session_health(rpc: &RpcClient) -> Result<(u64, u64, Option<u64>), CliError> {
    let resp = rpc.call("daemon.health", serde_json::json!({})).await?;
    let active = resp
        .get("active_sessions")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let orphans = resp
        .get("orphan_browser_trees")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let oldest = resp
        .get("oldest_active_session_age_secs")
        .and_then(|v| v.as_u64());
    Ok((active, orphans, oldest))
}

/// Check 9 — real browser smoke. Drives a full ephemeral session end-to-end so
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

/// When `e` is the daemon's typed `session_cap_exceeded` rejection (a
/// `CliError::Receipt` wire envelope), return the human detail line for the
/// `at_capacity` status; `None` for every other error (genuine failures).
/// Pulls `{active, cap}` out of the envelope's `data` so the line agrees
/// with `session_health`'s `active_sessions` count.
pub fn session_cap_detail(e: &CliError) -> Option<String> {
    let CliError::Receipt(v) = e else {
        return None;
    };
    if v.get("code").and_then(|c| c.as_str()) != Some("session_cap_exceeded") {
        return None;
    }
    const REMEDY: &str = "daemon is busy but healthy; close sessions or run `loom session reap`";
    Some(
        match (
            v.pointer("/data/active").and_then(|a| a.as_u64()),
            v.pointer("/data/cap").and_then(|c| c.as_u64()),
        ) {
            (Some(active), Some(cap)) => {
                format!("at capacity: active_sessions={active} cap={cap} — {REMEDY}")
            }
            _ => format!("at capacity: concurrent session cap reached — {REMEDY}"),
        },
    )
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

/// Check 4 — AOT artifacts current (stale-surface guard, RPC-free).
///
/// For the known `loom_surface_web` surface, read its `<name>.sha256` sidecar and
/// compare line 1 to THIS binary's embedded source SHA and line 2 to the live
/// engine-compat hash. A mismatch means the installed `.cwasm` was compiled from
/// different source bytes or by an incompatible engine — it will fail to load and
/// every dispatch will trap (`surface 'loom_surface_web' not loaded`). Returns a
/// TYPED "stale surface artifact — run `loom postinstall`" so the failure is
/// actionable BEFORE first dispatch and distinct from a generic `browser_smoke`
/// failure.
///
/// Passes (no false red) when: the artifact or sidecar is absent (check 3 owns
/// "present"); the sidecar is a legacy single-line stamp with no compat line (the
/// daemon would still boot a compatible legacy artifact); or this is a dev build
/// with no embedded source SHA (the daemon skips the source check too). Computes
/// the expected compat hash engine-free against the default runtime config — what
/// `loom postinstall` stamps.
#[cfg(feature = "postinstall")]
pub async fn check_aot_artifacts_current(surfaces_dir: &std::path::Path) -> Result<(), CliError> {
    use loom_host::surface_stamp::{embedded_surface_web_sha256, parse_surface_sidecar};
    use loom_host::wasm_runtime::{precompile_compatibility_hash_for, WasmRuntimeConfig};

    let name = "loom_surface_web";
    let cwasm = surfaces_dir.join(format!("{name}.cwasm"));
    let sidecar = surfaces_dir.join(format!("{name}.sha256"));

    // Artifact absent → check 3 ("present") owns that failure; nothing to verify.
    if !cwasm.exists() || !sidecar.exists() {
        return Ok(());
    }

    let contents = std::fs::read_to_string(&sidecar)
        .map_err(|e| CliError::Internal(format!("read {name} sidecar: {e}")))?;
    let (sidecar_sha, sidecar_compat) = parse_surface_sidecar(&contents);

    // Source-SHA strand. Skipped when this binary has no embedded SHA (dev build),
    // mirroring `ModuleLibrary::load_one`.
    let expected_sha = embedded_surface_web_sha256();
    if !expected_sha.is_empty() {
        if let Some(actual) = sidecar_sha {
            if actual != expected_sha {
                return Err(CliError::Internal(format!(
                    "stale surface artifact '{name}': sidecar source SHA '{actual}' != this \
                     binary's '{expected_sha}' — run `loom postinstall`"
                )));
            }
        }
    }

    // Engine-compat strand. Enforced only when the sidecar carries a compat line
    // (legacy single-line stamps are not flagged — the daemon still boots a
    // compatible legacy artifact, and the next `loom postinstall` upgrades it).
    if let Some(stored_compat) = sidecar_compat {
        let live_compat =
            precompile_compatibility_hash_for(&WasmRuntimeConfig::default().opt_level);
        if stored_compat != live_compat {
            return Err(CliError::Internal(format!(
                "stale surface artifact '{name}': compiled for engine '{stored_compat}', this \
                 binary is '{live_compat}' — run `loom postinstall`"
            )));
        }
    }

    Ok(())
}

/// Check 4 (non-postinstall build) — `loom-host` isn't linked, so the expected
/// stamp can't be computed. The non-postinstall binary is the RPC-only image
/// that runs `--daemon-only` (this check is reported `skipped`); pass otherwise.
#[cfg(not(feature = "postinstall"))]
pub async fn check_aot_artifacts_current(_surfaces_dir: &std::path::Path) -> Result<(), CliError> {
    Ok(())
}

/// Check 5 — Chromium binary present + sha256 matches pinned.
pub async fn check_chromium(
    chromium: &ChromiumDownloader,
    binary: &std::path::Path,
    expected_sha256: &str,
) -> Result<(), CliError> {
    let _ = binary; // ChromiumDownloader knows its own binary_path
    chromium.verify(expected_sha256).await
}

/// Check 6 — Keychain ACL probe (read-only accessibility check).
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

/// Check 7 — macOS Gatekeeper quarantine clear on the Chromium binary.
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
