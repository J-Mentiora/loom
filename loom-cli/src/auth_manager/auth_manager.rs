// AuthManager — reads the daemon HELLO token from the per-startup
// artefact.
//
// # Contract semantics
// - **HELLO token** lives at
//   `~/Library/Application Support/loom/auth/hello.token`. The daemon
//   writes it on startup and removes it on shutdown.
// - **Never persists across daemon restarts.** Stale tokens are
//   detected by verifying daemon liveness via the PID file (also in
//   `~/.../auth/`). If the PID is dead, the artefact is treated as
//   stale and `read_hello_token` returns `CliError::Connection(AuthFailed)`.
// - **Read-only.** This module never writes the token.
// - **Linux paths** mirror via `$XDG_DATA_HOME/loom/auth/`.

use std::path::PathBuf;

use crate::CliError;

/// Resolved auth artefact paths. Computed once at startup; immutable.
#[derive(Debug, Clone)]
pub struct AuthPaths {
    /// Absolute path to `hello.token`.
    pub token_path: PathBuf,
    /// Absolute path to `daemon.pid`.
    pub pid_path: PathBuf,
}

/// Stateless handle. Construct once per CLI invocation.
pub struct AuthManager {
    pub(crate) paths: AuthPaths,
}

impl AuthManager {
    /// Construct from resolved paths (typically via `ConfigResolver`).
    pub fn new(paths: AuthPaths) -> Self {
        Self { paths }
    }

    /// Reads the HELLO token from disk. Differentiates two failure
    /// modes that previously collapsed to `AuthFailed`:
    ///   - **No daemon running at all** (PID file missing or pointing
    ///     at a dead process) → `DaemonNotRunning` with the actionable
    ///     "Try: loom serve" message. Stale `auth/` files are best-effort
    ///     cleaned so the next `loom serve` startup isn't tripped by
    ///     them.
    ///   - **Daemon alive but token file unreadable / missing** →
    ///     `AuthFailed` (the legitimate "HELLO mismatch" case — daemon
    ///     restarted in flight, token was rotated, etc.).
    pub fn read_hello_token(&self) -> Result<String, CliError> {
        if !self.daemon_alive() {
            // Stale PID file — clean up so `loom serve` can start
            // fresh and so subsequent commands don't keep stat'ing
            // a dead PID. Best-effort: ignore unlink errors (the
            // file may have been removed already, or the user may
            // not have write access in some setups).
            let _ = std::fs::remove_file(&self.paths.pid_path);
            let _ = std::fs::remove_file(&self.paths.token_path);
            return Err(CliError::Connection(
                crate::error_mapper::ConnectionError::DaemonNotRunning,
            ));
        }
        std::fs::read_to_string(&self.paths.token_path)
            .map(|s| s.trim().to_string())
            .map_err(|_| CliError::Connection(crate::error_mapper::ConnectionError::AuthFailed))
    }

    /// Returns the absolute token path. Used by `DoctorRunner` for
    /// the artefact-presence check.
    pub fn token_path(&self) -> &std::path::Path {
        &self.paths.token_path
    }

    /// Verifies the daemon process is alive by `kill(pid, 0)` (POSIX
    /// liveness probe). Returns `false` for missing or stale PID files.
    ///
    /// Implementation: spawns `kill -0 <pid>` with stderr **redirected
    /// to /dev/null** so the system `kill`'s "No such process"
    /// diagnostic doesn't leak to the operator's terminal. Without that
    /// redirection, every stale-PID check would dump an extra confusing
    /// line of output ahead of the actual CLI error message.
    pub fn daemon_alive(&self) -> bool {
        let Ok(content) = std::fs::read_to_string(&self.paths.pid_path) else {
            return false;
        };
        let Ok(pid) = content.trim().parse::<u32>() else {
            return false;
        };
        // Reject PID 0 + PID 1 — `kill -0 0` and `kill -0 1` succeed
        // on POSIX systems but neither corresponds to a real loom
        // daemon. (PID 1 = init/launchd; PID 0 is the "send to every
        // process in the group" sentinel.)
        if pid == 0 || pid == 1 {
            return false;
        }
        // POSIX kill(pid, 0) via system kill binary.
        // Stderr → /dev/null so a "No such process" diagnostic doesn't
        // leak to the user (the CLI surfaces its own typed error).
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

/// Artefact paths inside an explicit auth dir — the resolved
/// `CliConfig.auth_dir` (config.toml `auth_dir` / `LOOM_AUTH_DIR`). The
/// daemon writes `hello.token` + `daemon.pid` under `<data_root>/auth/`,
/// so a custom `--data-root` daemon pairs with `auth_dir = <root>/auth`.
pub fn auth_paths_in(auth_dir: &std::path::Path) -> AuthPaths {
    AuthPaths {
        token_path: auth_dir.join("hello.token"),
        pid_path: auth_dir.join("daemon.pid"),
    }
}

/// Default platform paths. macOS: `~/Library/Application Support/loom/auth/`.
/// Linux: `$XDG_DATA_HOME/loom/auth/`.
/// Must agree with `CliConfig`'s compiled `auth_dir` default and the
/// daemon's `<data_root>/auth/` layout.
pub fn default_auth_paths() -> Result<AuthPaths, CliError> {
    let auth_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("loom/auth");
    Ok(auth_paths_in(&auth_dir))
}
