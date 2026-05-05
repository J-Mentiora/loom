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

    /// Reads the HELLO token from disk. Verifies the daemon is alive
    /// (via PID file) before returning the token; otherwise returns
    /// `CliError::Connection(AuthFailed)`.
    pub fn read_hello_token(&self) -> Result<String, CliError> {
        if !self.daemon_alive() {
            return Err(CliError::Connection(
                crate::error_mapper::ConnectionError::AuthFailed,
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
    /// liveness probe). Returns `false` for stale PID files.
    pub fn daemon_alive(&self) -> bool {
        let Ok(content) = std::fs::read_to_string(&self.paths.pid_path) else {
            return false;
        };
        let Ok(pid) = content.trim().parse::<u32>() else {
            return false;
        };
        // POSIX kill(pid, 0) via system kill binary.
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

/// Default platform paths. macOS: `~/Library/Application Support/loom/auth/`.
/// Linux: `$XDG_DATA_HOME/loom/auth/`.
pub fn default_auth_paths() -> Result<AuthPaths, CliError> {
    let auth_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("loom/auth");
    Ok(AuthPaths {
        token_path: auth_dir.join("hello.token"),
        pid_path: auth_dir.join("daemon.pid"),
    })
}
