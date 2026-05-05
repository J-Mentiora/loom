// Re-export of the locked v5.3 interface. DO NOT EDIT here.
// Edit `systems/loom-cli/modules/ServeRunner/interfaces.rs` instead.
// ServeRunner — `loom serve` local action.
//
// # Contract semantics
// - **Spawn.** Spawns `loom-daemon` as a subprocess with the
//   resolved socket path; reads daemon's HELLO token from stdout once
//   and prints to user. Token never persisted across daemon restarts;
//   `AuthManager` re-reads the per-startup artefact every invocation.
// - **RPC-free.** One of the 3 local-action exceptions.
// - **SLA.** Socket ready within 200 ms (per loom-cli contract).

use std::path::PathBuf;

use crate::CliError;

/// `loom serve` arguments + resolved options.
#[derive(Debug, Clone)]
pub struct ServeOptions {
    /// Resolved socket path. Default: platform-specific
    /// (`~/Library/Caches/loom/loom.sock` on macOS).
    pub socket_path: PathBuf,
    /// Optional config path forwarded to the daemon process.
    pub config_path: Option<PathBuf>,
    /// Daemon binary path. Default: same dir as the `loom` binary.
    pub daemon_binary: PathBuf,
}

/// Run the serve command. Spawns the daemon, reads HELLO from its
/// stdout once, prints to stdout, returns on success. Long-lived
/// daemon process detaches; the CLI process exits 0 once the HELLO
/// is observed.
pub async fn serve(opts: ServeOptions) -> Result<HelloDisclosure, CliError> {
    use tokio::io::AsyncBufReadExt as _;
    let mut cmd = tokio::process::Command::new(&opts.daemon_binary);
    cmd.arg("--socket").arg(&opts.socket_path);
    if let Some(cfg) = &opts.config_path {
        cmd.arg("--config").arg(cfg);
    }
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::inherit());
    let mut child = cmd
        .spawn()
        .map_err(|e| CliError::Internal(format!("spawn daemon: {e}")))?;
    let pid = child
        .id()
        .ok_or_else(|| CliError::Internal("daemon exited immediately".to_string()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CliError::Internal("no stdout from daemon".to_string()))?;
    let mut reader = tokio::io::BufReader::new(stdout).lines();
    let token = loop {
        let line = tokio::time::timeout(std::time::Duration::from_millis(5000), reader.next_line())
            .await
            .map_err(|_| {
                CliError::Connection(crate::error_mapper::ConnectionError::ConnectionTimeout)
            })?
            .map_err(|e| CliError::Internal(format!("read daemon stdout: {e}")))?
            .ok_or_else(|| CliError::Internal("daemon closed stdout".to_string()))?;
        if let Some(tok) = line.strip_prefix("HELLO_TOKEN=") {
            break tok.trim().to_string();
        }
    };
    println!("{}", format_hello_line(&token));
    Ok(HelloDisclosure {
        token,
        daemon_pid: pid,
        socket_path: opts.socket_path,
    })
}

/// Result of a successful spawn — the disclosed HELLO token plus the
/// daemon PID for `AuthManager` liveness checks.
#[derive(Debug, Clone)]
pub struct HelloDisclosure {
    pub token: String,
    pub daemon_pid: u32,
    pub socket_path: PathBuf,
}

/// Pure helper: format the HELLO disclosure for stdout.
/// Output is exactly `HELLO_TOKEN=<hex>\n` (single line).
pub fn format_hello_line(token: &str) -> String {
    format!("HELLO_TOKEN={token}\n")
}

/// Default daemon binary path resolution.
///
/// The cargo-install path drops the daemon into
/// `dirs::data_local_dir()/loom/bin/`, while brew/manual co-locate it next
/// to `loom`. `loom_shared::binary_resolver` walks both locations so the
/// daemon is found regardless of install method.
pub fn default_daemon_binary() -> Result<PathBuf, CliError> {
    loom_shared::binary_resolver::resolve_loom_sibling("loom-daemon").ok_or_else(|| {
        CliError::Internal(
            "loom-daemon binary not found in dirs::data_local_dir()/loom/bin/, \
             alongside the loom binary, or on PATH. \
             Run `loom postinstall` to fetch the missing binaries."
                .to_string(),
        )
    })
}
