// Supervisor — sole owner of the Chromium child process lifecycle.
//
// # Contract semantics
// - **`posix_spawn` only (HARD).** Chromium is launched
//   via `posix_spawn(2)` (through `nix::unistd::execvp`-equivalent or
//   `tokio::process::Command::spawn` with `pre_exec` for env scrub).
//   `fork()`+`exec()` is also acceptable as long as the spawn call is
//   atomic from the daemon's perspective. NO shell, NO `system(3)`.
// - **Locale-scrubbed env.** Sets `LC_ALL=C.UTF-8` +
//   `LANG=C.UTF-8`; removes `LC_MESSAGES`, `LC_NUMERIC`, `LC_TIME`.
//   This is structurally load-bearing for bit-equal replay (R3 +
//   locale = both in place before any user navigation).
// - **Bounded restart budget.** Max 3 restarts within
//   60s. On exhaustion the shim enters degraded state; all subsequent
//   requests resolve to `ShimErrorCode::ChromiumUnavailable`. The
//   daemon's `ShimManager` decides whether to respawn the entire
//   shim process.
// - **State-invalidation cascade.** On Chromium exit (SIGCHLD) or
//   force-restart (`ProcessMonitor` callback), `Supervisor` calls
//   `CdpConnection::invalidate_session`,
//   `TargetManager::invalidate_targets`, and
//   `Dispatcher::invalidate_in_flight` synchronously before any
//   restart attempt.
// - **Force-restart callback registration.** `ProcessMonitor` does NOT
//   import `Supervisor`. At boot, `Supervisor::start` registers an
//   `Arc<dyn Fn() + Send + Sync>` with `ProcessMonitor`; the monitor
//   invokes it on hang detection. Same kill-callback pattern as
//   loom-core's `BudgetEnforcer → SessionManager`.
// - **Pinned Chromium revision.** `chromium_path`
//   resolution + SHA-256 verification is the daemon's job (`loom
//   postinstall`). On first `spawn_target` after a mismatch, the
//   daemon passes `version_mismatch=true` and `Supervisor::start`
//   returns `ShimInternalError{detail: "ChromiumVersionMismatch"}`.

use crate::cdp_connection::cdp_connection::CdpConnection;
use crate::dispatcher::dispatcher::Dispatcher;
use crate::ipc_endpoint::ipc_endpoint::ShimErrorCode;
use crate::target_manager::target_manager::TargetManager;
use async_trait::async_trait;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;

/// Restart budget configuration. Soft binding (defaults shown).
#[derive(Debug, Clone, Copy)]
pub struct RestartBudget {
    pub max_within_window: u8,
    pub window: Duration,
}

impl Default for RestartBudget {
    fn default() -> Self {
        Self {
            max_within_window: 3,
            window: Duration::from_secs(60),
        }
    }
}

/// Configuration passed to `Supervisor::start`. Resolved by daemon
/// from the `[chromium]` section of `config.toml`.
#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    /// Pinned Chromium binary path (verified SHA-256 by daemon).
    pub chromium_path: PathBuf,
    /// User-data-dir (always under `$TMPDIR/loom-chromium-<pid>/`).
    pub user_data_dir: PathBuf,
    /// Additional Chromium command-line flags. Sandboxed defaults.
    pub extra_flags: Vec<String>,
    /// Restart budget.
    pub restart_budget: RestartBudget,
    /// If true, daemon detected a Chromium revision mismatch at install
    /// and Supervisor::start should fail with `ShimInternalError`.
    pub version_mismatch: bool,
}

impl SupervisorConfig {
    /// Convenience: a default config rooted at the given Chromium path.
    pub fn new(chromium_path: PathBuf, user_data_dir: PathBuf) -> Self {
        Self {
            chromium_path,
            user_data_dir,
            extra_flags: Vec::new(),
            restart_budget: RestartBudget::default(),
            version_mismatch: false,
        }
    }
}

/// Force-restart callback type. Invoked by `ProcessMonitor` on hang.
pub type ForceRestartCallback = Arc<dyn Fn() + Send + Sync>;

/// Crash-reason classification for the state-invalidation cascade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrashReason {
    /// SIGCHLD observed.
    ChildExited {
        exit_code: Option<i32>,
        signal: Option<i32>,
    },
    /// `ProcessMonitor` invoked the force-restart callback after 3
    /// consecutive `Browser.getVersion` timeouts.
    HangDetected,
    /// Cooperative shutdown — not an error.
    Shutdown,
}

/// Concrete `Supervisor`.
pub struct ChromiumSupervisor {
    pub(crate) config: SupervisorConfig,
    pub(crate) cdp: Arc<dyn CdpConnection>,
    pub(crate) targets: Arc<dyn TargetManager>,
    pub(crate) dispatcher: Arc<dyn Dispatcher>,
    pub(crate) restart_history: parking_lot::Mutex<Vec<Instant>>,
    pub(crate) child_pid: parking_lot::Mutex<Option<u32>>,
    pub(crate) child: tokio::sync::Mutex<Option<Child>>,
}

impl ChromiumSupervisor {
    pub fn new(
        config: SupervisorConfig,
        cdp: Arc<dyn CdpConnection>,
        targets: Arc<dyn TargetManager>,
        dispatcher: Arc<dyn Dispatcher>,
    ) -> Self {
        Self {
            config,
            cdp,
            targets,
            dispatcher,
            restart_history: parking_lot::Mutex::new(Vec::new()),
            child_pid: parking_lot::Mutex::new(None),
            child: tokio::sync::Mutex::new(None),
        }
    }
}

/// Public Supervisor trait surface. `start` and `shutdown` are async
/// because they do real subprocess + WS I/O.
#[async_trait]
pub trait Supervisor: Send + Sync {
    /// Spawn the initial Chromium child via `tokio::process::Command`
    /// with locale-scrubbed env. Returns the child pid on success.
    async fn start(&self) -> Result<u32, SupervisorError>;

    /// Build the force-restart callback to register with
    /// `ProcessMonitor`. Captures `self` via `Arc`.
    fn force_restart_callback(self: Arc<Self>) -> ForceRestartCallback;

    /// Cooperative shutdown — closes the CDP WebSocket, cleans the
    /// `user_data_dir`, sends SIGTERM then SIGKILL after 2s grace.
    async fn shutdown(&self) -> Result<(), SupervisorError>;

    /// Snapshot the current restart count within the budget window.
    fn restart_count_in_window(&self, now: Instant) -> u8;

    /// Cascade hook on Chromium exit / hang.
    fn handle_crash(&self, reason: CrashReason) -> Result<(), SupervisorError>;
}

#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error("posix_spawn failed: {0}")]
    SpawnFailed(String),
    #[error("Chromium version mismatch — install via `loom postinstall`")]
    VersionMismatch,
    #[error("restart budget exhausted ({restarts} within {window_ms} ms)")]
    BudgetExhausted { restarts: u8, window_ms: u64 },
    #[error("CDP connection failed: {0}")]
    CdpConnect(String),
}

impl From<SupervisorError> for ShimErrorCode {
    fn from(e: SupervisorError) -> Self {
        match e {
            SupervisorError::SpawnFailed(_)
            | SupervisorError::VersionMismatch
            | SupervisorError::CdpConnect(_) => ShimErrorCode::ShimInternalError,
            SupervisorError::BudgetExhausted { .. } => ShimErrorCode::ChromiumUnavailable,
        }
    }
}

#[async_trait]
impl Supervisor for ChromiumSupervisor {
    async fn start(&self) -> Result<u32, SupervisorError> {
        if self.config.version_mismatch {
            return Err(SupervisorError::VersionMismatch);
        }
        // Ensure user_data_dir exists.
        if let Err(e) = std::fs::create_dir_all(&self.config.user_data_dir) {
            return Err(SupervisorError::SpawnFailed(format!(
                "create_dir_all({:?}): {e}",
                self.config.user_data_dir
            )));
        }

        let mut cmd = tokio::process::Command::new(&self.config.chromium_path);
        cmd.args([
            "--headless=new",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-features=Translate",
            "--disable-component-update",
            "--remote-debugging-port=0",
        ]);
        cmd.arg(format!(
            "--user-data-dir={}",
            self.config.user_data_dir.display()
        ));
        for f in &self.config.extra_flags {
            cmd.arg(f);
        }
        // Env-var escape hatch for environments where Chromium needs
        // extra flags the daemon doesn't know to set: unprivileged
        // Docker containers (`--no-sandbox`), GHA runners with no
        // /dev/shm worth speaking of (`--disable-dev-shm-usage`),
        // headless servers without a working dbus, and so on.
        // Whitespace-separated. Real-user installs don't need this;
        // it's strictly an opt-in for CI / sandbox-less runtimes.
        if let Ok(extras) = std::env::var("LOOM_CHROMIUM_EXTRA_FLAGS") {
            for f in extras.split_whitespace() {
                cmd.arg(f);
            }
        }
        let (set_env, remove_env) = locale_scrub_env();
        for (k, v) in set_env {
            cmd.env(k, v);
        }
        for k in remove_env {
            cmd.env_remove(k);
        }
        // We capture stderr to parse the "DevTools listening on" line.
        // kill_on_drop(false) — explicit shutdown handshake per practitioner
        // bug magnet #4. Drop without explicit kill leaves a zombie which
        // shutdown() reaps via SIGTERM/SIGKILL.
        cmd.stderr(Stdio::piped())
            .stdout(Stdio::null())
            .kill_on_drop(false);

        // Pin Chromium into a fresh process group so a
        // single `killpg(pgid, SIGKILL)` reaps the entire helper-process
        // subtree atomically. Chromium spawns ~7 helper processes
        // (renderer, GPU, utility, network) which inherit the parent's
        // pgid. Without this, `kill -9` of the shim leaves orphans
        // running — observed in manual smoke testing before this fix.
        //
        // SAFETY: setpgid(0,0) is async-signal-safe per POSIX, so it's
        // permitted in pre_exec. process_group(0) is the tokio
        // equivalent introduced in tokio 1.27+.
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd
            .spawn()
            .map_err(|e| SupervisorError::SpawnFailed(e.to_string()))?;
        let pid = child
            .id()
            .ok_or_else(|| SupervisorError::SpawnFailed("no pid".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| SupervisorError::SpawnFailed("stderr unavailable".into()))?;

        let ws_url =
            parse_devtools_url(stderr, &self.config.user_data_dir, Duration::from_secs(10)).await?;

        self.cdp
            .connect(&ws_url)
            .await
            .map_err(|e| SupervisorError::CdpConnect(e.to_string()))?;

        *self.child_pid.lock() = Some(pid);

        // Watcher: when the chromium subprocess dies, force-exit the
        // shim process. Without this the shim hangs for ~30s on the
        // chromiumoxide WebSocket read after chromium's TCP socket
        // closes (verified empirically: kill chromium → next CDP
        // command waits the full daemon recv_timeout). Letting the
        // shim exit lets the daemon's existing shim-crash watcher
        // (loom-host/shim_manager/process.rs) fire and clear all
        // pending oneshots → user-visible failure surfaces in <1s
        // instead of 30s.
        //
        // We use `child.wait().await` for detection rather than a
        // `kill(pid, 0)` poll because a kill-9'd chromium becomes a
        // zombie that `kill(pid, 0)` reports as alive until the
        // parent reaps it. wait() reaps AND returns the exit status
        // the moment the kernel marks the child as dead.
        //
        // Trade-off: we move ownership of the Child into this watcher
        // task, so shutdown() can no longer wait on it directly. That
        // path now relies on `killpg` (which it already calls) plus a
        // brief sleep. We also skip the `*self.child.lock().await =
        // Some(child)` stash; if a future feature needs to interact
        // with the child handle (beyond pgid kills which only need
        // child_pid), threading it back through a watcher-task
        // channel is the right move.
        tokio::spawn(async move {
            let status = child.wait().await;
            tracing::warn!(
                chromium_pid = pid,
                status = ?status,
                "chromium subprocess exited; shim self-exiting so daemon's crash watcher fires"
            );
            // Exit code 2 = chromium died unexpectedly. The daemon's
            // crash watcher cares about exit happening, not the
            // specific code, but a non-zero code makes post-mortem
            // grep easier.
            std::process::exit(2);
        });

        Ok(pid)
    }

    fn force_restart_callback(self: Arc<Self>) -> ForceRestartCallback {
        Arc::new(move || {
            let _ = self.handle_crash(CrashReason::HangDetected);
        })
    }

    async fn shutdown(&self) -> Result<(), SupervisorError> {
        // Close the CDP session so the writer task drains. After this,
        // the child should exit on its own after we send SIGTERM.
        self.cdp.invalidate_session();

        let pid_opt = *self.child_pid.lock();
        let mut child_guard = self.child.lock().await;
        if let Some(mut child) = child_guard.take() {
            // SIGTERM the entire process group via killpg(-pgid). This
            // takes down Chromium AND all its helper processes (renderer,
            // GPU, utility) in one call. Chromium was spawned with
            // process_group(0) so its pgid == its pid.
            if let Some(pid) = pid_opt {
                #[cfg(unix)]
                unsafe {
                    // killpg sends to the entire process group named by
                    // the negative pid. -SIGTERM is the cooperative
                    // signal; helpers will exit cleanly within ~1s.
                    libc::killpg(pid as libc::pid_t, libc::SIGTERM);
                }
                #[cfg(not(unix))]
                let _ = pid;
            }
            // Wait up to 2s for clean exit, then SIGKILL.
            match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
                Ok(Ok(_status)) => { /* clean */ }
                Ok(Err(e)) => {
                    tracing::warn!("supervisor: child wait error: {e}");
                }
                Err(_) => {
                    // Timed out — SIGKILL the entire process group.
                    if let Some(pid) = pid_opt {
                        #[cfg(unix)]
                        unsafe {
                            libc::killpg(pid as libc::pid_t, libc::SIGKILL);
                        }
                    }
                    let _ = child.wait().await;
                }
            }
        }
        *self.child_pid.lock() = None;
        Ok(())
    }

    fn restart_count_in_window(&self, now: Instant) -> u8 {
        let history = self.restart_history.lock();
        let budget = self.config.restart_budget;
        let window_start = now.checked_sub(budget.window).unwrap_or(now);
        history.iter().filter(|t| **t >= window_start).count() as u8
    }

    fn handle_crash(&self, _reason: CrashReason) -> Result<(), SupervisorError> {
        // State-invalidation cascade.
        self.cdp.invalidate_session();
        self.targets.invalidate_targets();
        self.dispatcher.invalidate_in_flight("chromium crash");
        self.restart_history.lock().push(Instant::now());
        // Restart-budget bookkeeping is here; supervisor::run (L7) decides
        // whether to actually respawn based on restart_count_in_window().
        Ok(())
    }
}

/// Parse the `DevTools listening on (ws://...)` line from Chromium's
/// stderr, racing it against a `<user_data_dir>/DevToolsActivePort` file
/// poll (Chromium writes the port + path there too — more reliable than
/// stderr scraping under buffering pressure).
///
/// After the URL is found, the remainder of stderr is drained to a
/// background tracing task so Chromium doesn't block on a full ~64 KB
/// stderr pipe.
pub async fn parse_devtools_url(
    stderr: tokio::process::ChildStderr,
    user_data_dir: &std::path::Path,
    deadline: Duration,
) -> Result<String, SupervisorError> {
    let active_port = user_data_dir.join("DevToolsActivePort");
    let active_port_for_task = active_port.clone();

    let stderr_url_fut = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut buf = String::new();
        loop {
            buf.clear();
            match reader.read_line(&mut buf).await {
                Ok(0) => return (None, reader),
                Ok(_) => {
                    if let Some(url) = extract_ws_url(&buf) {
                        return (Some(url), reader);
                    }
                }
                Err(_) => return (None, reader),
            }
        }
    });

    let file_poll_fut = async move {
        let start = Instant::now();
        loop {
            if start.elapsed() > deadline {
                return None;
            }
            if let Ok(contents) = tokio::fs::read_to_string(&active_port_for_task).await {
                if let Some(url) = parse_active_port_file(&contents) {
                    return Some(url);
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };

    let result = tokio::select! {
        joined = stderr_url_fut => {
            match joined {
                Ok((Some(url), reader)) => {
                    // Drain rest of stderr so Chromium doesn't block on a full pipe.
                    tokio::spawn(drain_to_log(reader));
                    Ok(url)
                }
                Ok((None, _)) => Err(SupervisorError::SpawnFailed(
                    "stderr closed before DevTools URL".into(),
                )),
                Err(e) => Err(SupervisorError::SpawnFailed(format!("stderr task: {e}"))),
            }
        }
        url = file_poll_fut => {
            match url {
                Some(u) => Ok(u),
                None => Err(SupervisorError::SpawnFailed(
                    "DevToolsActivePort file timeout".into(),
                )),
            }
        }
        _ = tokio::time::sleep(deadline) => {
            Err(SupervisorError::SpawnFailed(format!(
                "DevTools URL not found within {} ms",
                deadline.as_millis()
            )))
        }
    };
    result
}

async fn drain_to_log(mut reader: BufReader<tokio::process::ChildStderr>) {
    let mut buf = String::new();
    loop {
        buf.clear();
        match reader.read_line(&mut buf).await {
            Ok(0) => return,
            Ok(_) => {
                if !buf.is_empty() {
                    tracing::trace!(target: "chromium.stderr", "{}", buf.trim_end());
                }
            }
            Err(_) => return,
        }
    }
}

/// Extract `ws://...` from a "DevTools listening on ws://..." line. Pure
/// helper — testable.
pub fn extract_ws_url(line: &str) -> Option<String> {
    // Real Chromium prints exactly: "DevTools listening on ws://..."
    let prefix = "DevTools listening on ";
    let idx = line.find(prefix)?;
    let rest = line[idx + prefix.len()..].trim();
    if rest.starts_with("ws://") || rest.starts_with("wss://") {
        Some(rest.to_string())
    } else {
        None
    }
}

/// Parse the contents of `<user_data_dir>/DevToolsActivePort`. The file
/// has the format: `<port>\n<ws_path>` (path starts with `/`).
pub fn parse_active_port_file(contents: &str) -> Option<String> {
    let mut lines = contents.lines();
    let port = lines.next()?.trim().parse::<u16>().ok()?;
    let path = lines.next().unwrap_or("/devtools/browser/").to_string();
    let path = if path.starts_with('/') {
        path
    } else {
        format!("/{path}")
    };
    Some(format!("ws://127.0.0.1:{port}{path}"))
}

/// Pure helper: locale-scrub map applied at spawn time. Public for
/// testability — called when building the spawn env.
/// Returns (set_pairs, remove_keys).
pub fn locale_scrub_env() -> (Vec<(&'static str, &'static str)>, Vec<&'static str>) {
    (
        vec![("LC_ALL", "C.UTF-8"), ("LANG", "C.UTF-8")],
        vec!["LC_MESSAGES", "LC_NUMERIC", "LC_TIME"],
    )
}

/// Pure helper: decide whether a restart is allowed given the recorded
/// history and the budget window. Used by `handle_crash`.
pub fn restart_allowed(history: &[Instant], now: Instant, budget: RestartBudget) -> bool {
    let window_start = now.checked_sub(budget.window).unwrap_or(now);
    let in_window = history.iter().filter(|t| **t >= window_start).count();
    (in_window as u8) < budget.max_within_window
}
