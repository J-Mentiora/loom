// ErrorMapper — SOLE owner of `CliError` and exit-code mapping.
//
// # Contract semantics
// - Exit codes 0/1/2/3/4/5/6 are owned exclusively here.
//   `Ok→0`, `CliError::Receipt(error)→1`, `CliError::Connection→1`,
//   `CliError::SupplyChain→1`, `CliError::DoctorFailed→1`,
//   `CliError::Usage→2`, `CliError::Internal→2`,
//   `CliError::Config→3`, `CliError::Protocol→4`,
//   `CliError::SurfaceUnavailable→5`, `CliError::SessionsDiffer→6`.
//   Arbitrary codes (127 etc.) banned. Clippy lint
//   `// FORBIDDEN: std::process::exit outside main + ErrorMapper`
//   forbids `std::process::exit` outside `main` and this module.
// - `From<RpcError> for CliError` mirrors
//   `LoomErrorCode` 1:1; `tools/lint-error-codes.py` walks the
//   codegen output and asserts every variant has a matching arm.
// - **Actionable error messages.**
//   `DaemonNotRunning → "Try: loom serve"` etc. The full catalog is
//   pinned in `connection_message`.

use serde::{Deserialize, Serialize};

/// The CLI's typed error enum. Constructed only by handlers; consumed
/// only by `ErrorMapper::map_exit_code` and the `main` exit path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CliError {
    /// Usage error — caught BEFORE any RPC call. Exit 2.
    Usage(String),
    /// Typed error receipt from the daemon. Exit 1. Receipt flows
    /// through verbatim.
    Receipt(serde_json::Value),
    /// Connection failure. Exit 1 with actionable message.
    Connection(ConnectionError),
    /// Chromium / WASM supply-chain integrity failure. Exit 1.
    SupplyChain {
        expected_hash: String,
        actual_hash: String,
        url: String,
    },
    /// Doctor health check failure. Exit 1 with structured report.
    DoctorFailed(DoctorReport),
    /// Internal bug. Exit 2.
    Internal(String),
    /// Operation requires elevated privileges; caller degrades gracefully. Exit 0.
    /// Used by `postinstall_runner::plist_step` when the launchd plist write is
    /// denied due to the user not running as root.
    PermissionDenied(String),
    /// The requested surface is not loaded in the daemon. Exit 5.
    /// Mapped from `LoomErrorCode::SurfaceUnavailable` (RPC wire code
    /// `"surface_unavailable"`) by `From<RpcError> for CliError`.
    SurfaceUnavailable(String),
    /// Configuration resolution failure. Exit 3.
    /// Reserved for future emitters in `cli_config::resolve` and friends;
    /// the mapping is wired now so the table is testable today.
    Config(String),
    /// JSON-RPC protocol-level failure (malformed envelope, schema skew
    /// mid-call, etc). Exit 4. Reserved for future emitters
    /// in `rpc_client`; the mapping is wired now so the table is testable today.
    Protocol(String),
    /// `loom session diff` found structural differences between two sessions.
    /// Exit 6. Distinct from `Internal` because diverging
    /// sessions are an *expected* result, not a CLI usage bug — Unix-`diff(1)`
    /// precedent (0 = same, non-0 = differ, 2 = error). The carried String
    /// is a one-line summary for stderr (e.g. "3 field diffs, action_count_delta=1").
    SessionsDiffer(String),
    /// Chromium binary could not be located by the resolver.
    /// Exit 1 with a platform-aware actionable install message. Mapped from
    /// `LoomErrorCode::BrowserNotFound` (RPC wire code `"browser_not_found"`)
    /// by `From<RpcError> for CliError`.
    BrowserNotFound(String),
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CliError::Usage(msg) => write!(f, "Error: {msg}"),
            CliError::Receipt(v) => {
                // Two receipt shapes flow through CliError::Receipt:
                //
                //   1. RPC-error envelope: {code, message, data?}
                //      (from JsonRpcError::from_loom_error → CLI receipt-error path).
                //   2. Action receipt with status="error":
                //      {status: "error", error: {kind, detail}, ...}
                //      (URL allowlist rejection, profile-restricted evaluate,
                //       safe-profile downloads, etc.).
                //
                // For shape 2 the top-level code/message are absent and the
                // error info lives in error.kind / error.detail. Falling back
                // to "unknown error" loses that information; instead, prefer
                // error.kind as the code and the action method name as the
                // surface tag (when available).
                let code = v.get("code").and_then(|c| c.as_str()).or_else(|| {
                    v.get("error")
                        .and_then(|e| e.get("kind"))
                        .and_then(|k| k.as_str())
                });
                // For action-receipt shape 2, prefer the most informative
                // message we can build from error.detail. Three sub-shapes:
                //   - error.detail = "string" (URL allowlist): use verbatim.
                //   - error.detail = {chromium_error, url}: synthesize
                //     "<chromium_error> for <url>".
                //   - error.detail = {status_code, url}: synthesize
                //     "HTTP <status_code> from <url>".
                // Falls through to top-level `message` for shape 1.
                let detail_msg = v.get("error").and_then(|e| e.get("detail")).and_then(|d| {
                    if let Some(s) = d.as_str() {
                        Some(s.to_string())
                    } else if let Some(o) = d.as_object() {
                        let url = o.get("url").and_then(|u| u.as_str());
                        if let Some(ce) = o.get("chromium_error").and_then(|c| c.as_str()) {
                            Some(match url {
                                Some(u) => format!("{ce} for {u}"),
                                None => ce.to_string(),
                            })
                        } else if let Some(sc) = o.get("status_code") {
                            Some(match url {
                                Some(u) => format!("HTTP {sc} from {u}"),
                                None => format!("HTTP {sc}"),
                            })
                        } else if let Some(reason) =
                            o.get("reason").and_then(|r| r.as_str())
                        {
                            // Generic typed-detail with a `reason` string —
                            // e.g. js_throw / profile_restricted /
                            // selector-miss receipts. Append `verb` /
                            // `matched_pattern` when present so the message
                            // surfaces actionable context without forcing
                            // the user to read the structured-detail line.
                            let mut parts = vec![reason.to_string()];
                            if let Some(verb) = o.get("verb").and_then(|v| v.as_str()) {
                                parts.push(format!("({verb})"));
                            }
                            if let Some(p) =
                                o.get("matched_pattern").and_then(|v| v.as_str())
                            {
                                parts.push(format!("[matched: {p}]"));
                            }
                            Some(parts.join(" "))
                        } else if let Some(matched) =
                            o.get("matched_pattern").and_then(|m| m.as_str())
                        {
                            // Profile-restricted receipts: detail has
                            // `matched_pattern` + `profile` + `violation`
                            // but no `reason`. Synthesize a clear message.
                            let profile = o
                                .get("profile")
                                .and_then(|p| p.as_str())
                                .unwrap_or("unknown");
                            Some(format!(
                                "matched denylist pattern '{matched}' under profile '{profile}'"
                            ))
                        } else {
                            // Page-side js_throw exception text (multi-line)
                            // — surface the first line as the message.
                            o.get("exception")
                                .and_then(|e| e.as_str())
                                .map(|exc| exc.lines().next().unwrap_or(exc).to_string())
                        }
                    } else {
                        None
                    }
                });
                let message_owned = v
                    .get("message")
                    .and_then(|m| m.as_str())
                    .map(|s| s.to_string())
                    .or(detail_msg)
                    .unwrap_or_else(|| "unknown error".to_string());
                let message = message_owned.as_str();
                match code {
                    Some(c) => write!(f, "Error: {c}: {message}")?,
                    None => write!(f, "Error: {message}")?,
                }
                if let Some(data) = v.get("data") {
                    if !data.is_null() {
                        write!(f, "\n  data: {data}")?;
                    }
                }
                // Surface error.detail as a structured object too if it's
                // not a string (e.g. http_status carries {url, status_code}).
                if let Some(detail) = v.get("error").and_then(|e| e.get("detail")) {
                    if !detail.is_null() && !detail.is_string() {
                        write!(f, "\n  detail: {detail}")?;
                    }
                }
                Ok(())
            }
            CliError::Connection(e) => write!(f, "{}", connection_message(e)),
            CliError::SupplyChain {
                expected_hash,
                actual_hash,
                url,
            } => write!(
                f,
                "Error: supply chain integrity failure for {url}\n  expected: {expected_hash}\n  actual:   {actual_hash}"
            ),
            CliError::DoctorFailed(report) => {
                write!(f, "Error: doctor health check failed")?;
                for failure in &report.failures {
                    write!(f, "\n  - {failure}")?;
                }
                Ok(())
            }
            CliError::Internal(msg) => write!(f, "Error: {msg}"),
            // PermissionDenied exits 0 (graceful degrade). Informational only.
            CliError::PermissionDenied(msg) => write!(f, "Warning: {msg} (skipped)"),
            CliError::SurfaceUnavailable(msg) => {
                write!(f, "Error: surface unavailable — {msg}")
            }
            CliError::Config(msg) => write!(f, "Error: config: {msg}"),
            CliError::Protocol(msg) => write!(f, "Error: protocol: {msg}"),
            CliError::SessionsDiffer(msg) => write!(f, "Error: sessions differ — {msg}"),
            // Platform-aware actionable install command.
            // The carried `_msg` is the daemon-side detail; we render a fixed
            // user-facing message keyed off the host OS so the install hint
            // matches the user's package manager.
            CliError::BrowserNotFound(_msg) => {
                if cfg!(target_os = "macos") {
                    write!(
                        f,
                        "Error: Chromium not found. \
                         Install via 'brew install --cask chromium', \
                         then run 'loom doctor'."
                    )
                } else if cfg!(target_os = "linux") {
                    write!(
                        f,
                        "Error: Chromium not found. \
                         Install via your distro's package manager: \
                         'apt install chromium-browser' (Debian/Ubuntu), \
                         'dnf install chromium' (Fedora/RHEL), or \
                         'pacman -S chromium' (Arch). \
                         Then run 'loom doctor'."
                    )
                } else {
                    write!(
                        f,
                        "Error: Chromium not found. \
                         Install via 'brew install --cask chromium' (macOS) or \
                         your distro's package manager (Linux), \
                         then run 'loom doctor'."
                    )
                }
            }
        }
    }
}

/// Connection-failure subspecies. Each variant has a fixed actionable
/// message via `connection_message`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ConnectionError {
    /// → "Try: loom serve"
    DaemonNotRunning,
    /// → "Daemon unresponsive after 30s. Check `loom doctor`."
    ConnectionTimeout,
    /// → "HELLO mismatch. Daemon may have been restarted; rerun."
    AuthFailed,
    /// → "Daemon schema vN, CLI expected vM. Reinstall."
    SchemaVersionSkew,
}

/// Structured doctor report carried inside `CliError::DoctorFailed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
    pub failures: Vec<String>,
}

/// One row in the doctor report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

/// Map a `CliError` (or `Ok`) to a process exit code. SOLE caller of
/// `std::process::exit` apart from `main`.
pub fn map_exit_code(result: &Result<(), CliError>) -> i32 {
    match result {
        Ok(()) => EXIT_OK,
        Err(CliError::Usage(_)) => EXIT_USAGE,
        Err(CliError::Receipt(v)) => {
            if v.get("status").and_then(|s| s.as_str()) == Some("ok") {
                EXIT_OK
            } else {
                EXIT_RECEIPT_ERROR
            }
        }
        Err(CliError::Connection(_)) => EXIT_RECEIPT_ERROR,
        Err(CliError::SupplyChain { .. }) => EXIT_RECEIPT_ERROR,
        Err(CliError::DoctorFailed(_)) => EXIT_RECEIPT_ERROR,
        Err(CliError::Internal(_)) => EXIT_USAGE,
        // PermissionDenied is a graceful degrade — non-root plist
        // write is skipped, not fatal. Exit 0 so the postinstall receipt is "ok".
        Err(CliError::PermissionDenied(_)) => EXIT_OK,
        // SurfaceUnavailable: daemon has no surface loaded for this action. Exit 5.
        Err(CliError::SurfaceUnavailable(_)) => EXIT_SURFACE_UNAVAILABLE,
        // Config / Protocol mapping table.
        Err(CliError::Config(_)) => EXIT_CONFIG,
        Err(CliError::Protocol(_)) => EXIT_PROTOCOL,
        // SessionsDiffer: `loom session diff` found structural differences. Exit 6.
        Err(CliError::SessionsDiffer(_)) => EXIT_DIFFERS,
        // BrowserNotFound is exit 1 (consistent with other
        // prereq-missing errors like `Connection(DaemonNotRunning)`).
        Err(CliError::BrowserNotFound(_)) => EXIT_RECEIPT_ERROR,
    }
}

/// Inspect a daemon receipt JSON. If the top-level `status` field is the
/// string `"error"`, raise it to `Err(CliError::Receipt(v))` so it joins the
/// receipt-error class (exit 1). Otherwise (status absent, or any non-error
/// value) return the value verbatim.
pub fn receipt_to_result(v: serde_json::Value) -> Result<serde_json::Value, CliError> {
    if v.get("status").and_then(|s| s.as_str()) == Some("error") {
        Err(CliError::Receipt(v))
    } else {
        Ok(v)
    }
}

/// Format an error for display to the user. Returns `None` for `Ok(())`.
/// Pure function; used by `print_error` and by integration tests that need
/// to assert stderr content without process-level stderr capture.
pub fn format_error(result: &Result<(), CliError>) -> Option<String> {
    match result {
        Ok(()) => None,
        Err(e) => Some(e.to_string()),
    }
}

/// Print the error message for a CLI result to stderr.
/// Called by `main` before `map_exit_code` to ensure every error class
/// is surfaced to the user before process exit. Uses stderr so errors
/// never pollute the JSON receipt stream on stdout.
///
/// Color-agnostic version (used in early-init paths where no resolved
/// `CliConfig` exists yet). Plain bytes; no ANSI.
pub fn print_error(result: &Result<(), CliError>) {
    if let Some(msg) = format_error(result) {
        eprintln!("{msg}");
    }
}

/// Color-aware variant: when `stderr_color_enabled` is true,
/// the message is rendered RED+BOLD. Used after `CliConfig` resolution
/// in `cli_main`. The plain `print_error` is preserved for the early-init
/// failure path.
pub fn print_error_with_color(result: &Result<(), CliError>, stderr_color_enabled: bool) {
    if let Some(msg) = format_error(result) {
        if stderr_color_enabled {
            use crate::pretty_renderer::ansi;
            let style = ansi::combine(ansi::BOLD, ansi::RED);
            eprintln!("{}{}{}", style, msg, ansi::RESET);
        } else {
            eprintln!("{msg}");
        }
    }
}

/// Returns the actionable error message string for a connection
/// failure variant. Pure function.
pub fn connection_message(err: &ConnectionError) -> &'static str {
    match err {
        ConnectionError::DaemonNotRunning => "Error: Loom Daemon is not running. Try: loom serve",
        ConnectionError::ConnectionTimeout => {
            "Error: Daemon unresponsive after 30s. Check `loom doctor`."
        }
        ConnectionError::AuthFailed => {
            "Error: HELLO mismatch. Daemon may have been restarted; rerun the command."
        }
        ConnectionError::SchemaVersionSkew => {
            "Error: Daemon schema version mismatch. Reinstall loom-cli."
        }
    }
}

/// Constants asserted by the unit tests + the lint script.
pub const EXIT_OK: i32 = 0;
pub const EXIT_RECEIPT_ERROR: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
/// Surface not loaded in the daemon.
pub const EXIT_SURFACE_UNAVAILABLE: i32 = 5;
/// Configuration resolution failure.
pub const EXIT_CONFIG: i32 = 3;
/// JSON-RPC protocol-level failure.
pub const EXIT_PROTOCOL: i32 = 4;
/// `loom session diff` found structural differences.
pub const EXIT_DIFFERS: i32 = 6;
