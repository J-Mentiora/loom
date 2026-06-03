//! `DaemonTestHarness` — shared fixture for integration tests that need a real
//! `loom-daemon` subprocess (AC11 / FND-0006).
//!
//! # What it provides
//! - **Unique temp socket per test.** Each harness owns a private [`TempDir`]
//!   and the daemon binds `<tempdir>/loom.sock`. Two harnesses never share a
//!   socket, so daemon-backed tests run in parallel without colliding — the
//!   mitigation for the "daemon-fixture flake" risk called out in the plan
//!   architecture.
//! - **Hermetic HOME.** `HOME` and the `XDG_*` dirs point inside the tempdir, so
//!   the daemon's auth artefacts (`hello.token`, `daemon.pid`) and config never
//!   touch the developer's real environment, and are torn down with the tempdir.
//! - **start / stop.** [`DaemonTestHarness::start`] spawns the daemon and blocks
//!   until it is ready; [`DaemonTestHarness::stop`] (and `Drop`) kills and reaps
//!   it.
//! - **ready-wait.** `start` waits for the daemon's `HELLO_TOKEN=` line on a
//!   single bounded deadline — a timeout, never a retry loop (FND-0006 no-flake
//!   bar).
//!
//! The `loom-daemon` and `loom` binaries are resolved via Cargo's
//! `CARGO_BIN_EXE_*` env vars, so Cargo builds them before the test runs — no
//! manual build step and no `current_exe()` path-walking.
//!
//! ```ignore
//! mod common;
//! use common::daemon_test_harness::DaemonTestHarness;
//!
//! let mut harness = DaemonTestHarness::new();
//! harness.start(); // spawns loom-daemon, blocks until HELLO
//! let output = harness.loom_command().arg("doctor").output().unwrap();
//! // `harness` drop stops the daemon and removes the tempdir.
//! ```

#![allow(dead_code)]

use std::ffi::{OsStr, OsString};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use tempfile::TempDir;

/// Generous single-shot ceiling for the daemon to announce readiness. Mirrors
/// the keychain/naverr e2e fixtures (15s) — comfortable on a cold CI runner.
/// This is a timeout, never a retry: the daemon either says HELLO once inside
/// the window or the test fails loudly (FND-0006).
pub const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(15);

/// A real `loom-daemon` subprocess bound to a unique, throwaway Unix socket.
pub struct DaemonTestHarness {
    /// Owns the isolated filesystem root (socket + hermetic HOME). Dropping it
    /// removes the directory tree; declared last so it is torn down after the
    /// child process in `Drop`.
    tempdir: TempDir,
    socket_path: PathBuf,
    ready_timeout: Duration,
    extra_env: Vec<(OsString, OsString)>,
    /// `Some` once `start()` has spawned the daemon.
    child: Option<Child>,
    /// The HELLO token disclosed by the daemon on startup, captured by `start()`.
    hello_token: Option<String>,
}

impl DaemonTestHarness {
    /// Allocate an isolated harness **without** spawning the daemon: creates the
    /// private tempdir and computes the unique socket path. Cheap and
    /// daemon-free, so path/uniqueness assertions don't need a built binary.
    pub fn new() -> Self {
        let tempdir = TempDir::new().expect("create harness tempdir");
        // Socket directly under the tempdir root keeps the path short — Unix
        // socket paths are capped at ~104 bytes on macOS.
        let socket_path = tempdir.path().join("loom.sock");
        Self {
            tempdir,
            socket_path,
            ready_timeout: DEFAULT_READY_TIMEOUT,
            extra_env: Vec::new(),
            child: None,
            hello_token: None,
        }
    }

    /// Override the readiness deadline (default [`DEFAULT_READY_TIMEOUT`]).
    /// Builder-style; call before `start()`.
    pub fn with_ready_timeout(mut self, timeout: Duration) -> Self {
        self.ready_timeout = timeout;
        self
    }

    /// Add (or override) an environment variable applied to both the daemon and
    /// any [`loom_command`](Self::loom_command). Builder-style; call before
    /// `start()`. Later calls win, and these override the hermetic defaults.
    pub fn env(mut self, key: impl AsRef<OsStr>, val: impl AsRef<OsStr>) -> Self {
        self.extra_env
            .push((key.as_ref().to_owned(), val.as_ref().to_owned()));
        self
    }

    /// The unique Unix socket path for this harness. Valid before `start()`; the
    /// file itself only exists once the daemon has bound it.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// The hermetic `HOME` for this harness (the private tempdir root).
    pub fn home(&self) -> &Path {
        self.tempdir.path()
    }

    /// The HELLO token disclosed by the daemon. `Some` after a successful
    /// `start()`, `None` before start or after `stop()`.
    pub fn hello_token(&self) -> Option<&str> {
        self.hello_token.as_deref()
    }

    /// Whether the daemon subprocess is currently running.
    pub fn is_running(&self) -> bool {
        self.child.is_some()
    }

    /// Spawn the daemon and block until it announces readiness (`HELLO_TOKEN=`)
    /// or the ready-timeout elapses. Returns `&mut self` for chaining.
    ///
    /// # Panics
    /// - if called while a daemon is already running (`stop()` first), or
    /// - if the daemon fails to become ready within the configured timeout —
    ///   the panic message includes the daemon's captured stderr.
    pub fn start(&mut self) -> &mut Self {
        assert!(
            self.child.is_none(),
            "DaemonTestHarness::start called while a daemon is already running"
        );

        let stderr_path = self.tempdir.path().join("daemon.stderr");
        let stderr_file = std::fs::File::create(&stderr_path).expect("create daemon.stderr");

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_loom-daemon"));
        cmd.arg("--socket").arg(&self.socket_path);
        self.apply_hermetic_env(&mut cmd);
        cmd.stdout(Stdio::piped()).stderr(Stdio::from(stderr_file));

        let mut child = cmd.spawn().expect("spawn loom-daemon");
        let stdout = child.stdout.take().expect("daemon stdout piped");

        match read_hello_token(stdout, self.ready_timeout) {
            Ok(token) => {
                self.hello_token = Some(token);
                self.child = Some(child);
                self
            }
            Err(reason) => {
                let _ = child.kill();
                let _ = child.wait();
                let stderr_dump = std::fs::read_to_string(&stderr_path).unwrap_or_default();
                panic!(
                    "loom-daemon never became ready: {reason}\n\
                     --- daemon stderr ---\n{stderr_dump}"
                );
            }
        }
    }

    /// Build a `loom` CLI command pre-wired to this harness's socket and
    /// hermetic env. The caller adds args / configures stdio on the returned
    /// [`Command`]. Usable whether or not the daemon is running, though most
    /// verbs require a `start()`ed daemon.
    ///
    /// The socket is wired via the `LOOM_SOCKET_PATH` env var, NOT a `--socket`
    /// CLI flag: `--socket` is only defined on `loom serve` (see
    /// `command_router::ServeArgs`), so prepending it to an RPC subcommand like
    /// `session create` makes clap reject the whole invocation. The CLI's
    /// config resolver honours `LOOM_SOCKET_PATH` for every subcommand
    /// (`cli_config` env precedence: file → env → flag), so this is the correct
    /// override that also agrees with the daemon's `--socket` bind on both
    /// macOS and Linux.
    pub fn loom_command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_loom"));
        self.apply_hermetic_env(&mut cmd);
        cmd.env("LOOM_SOCKET_PATH", &self.socket_path);
        cmd
    }

    /// Stop the daemon if running (kill + reap). Idempotent; also invoked by
    /// `Drop`, so explicit calls are only needed to stop early or to assert the
    /// stopped state mid-test.
    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.hello_token = None;
    }

    /// Apply the hermetic HOME / XDG environment shared by the daemon and the
    /// CLI, then layer any caller `env(..)` overrides on top.
    fn apply_hermetic_env(&self, cmd: &mut Command) {
        let home = self.tempdir.path();
        cmd.env("HOME", home)
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("XDG_RUNTIME_DIR", home)
            // Keep tests off the real OS keychain by default; a test needing a
            // different backend overrides this via `.env(..)`.
            .env("LOOM_KEYCHAIN_BACKEND", "in_memory")
            .env("RUST_LOG", "warn");
        for (key, val) in &self.extra_env {
            cmd.env(key, val);
        }
    }
}

impl Default for DaemonTestHarness {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for DaemonTestHarness {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Read the daemon's stdout until the `HELLO_TOKEN=<hex>` readiness line
/// appears, returning the token. Fails on EOF, read error, or timeout.
///
/// The blocking read runs on a detached thread fed through a bounded
/// [`mpsc::Receiver::recv_timeout`], so a daemon that hangs mid-line (or never
/// prints HELLO) can't wedge the test past the deadline — a plain `read_line`
/// loop with an elapsed-time check could block forever inside a single read.
/// On timeout the thread is left blocked on `read_line`; killing the child in
/// the caller closes the pipe, which unblocks and ends it. Single-shot: there
/// is no retry (FND-0006).
fn read_hello_token(stdout: ChildStdout, timeout: Duration) -> Result<String, String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    let _ = tx.send(Err("stdout closed (EOF) before HELLO_TOKEN".to_string()));
                    return;
                }
                Ok(_) => {
                    if let Some(token) = line.strip_prefix("HELLO_TOKEN=") {
                        let _ = tx.send(Ok(token.trim().to_string()));
                        return;
                    }
                    // Any other line (banners, logs) is ignored; keep reading.
                }
                Err(e) => {
                    let _ = tx.send(Err(format!("stdout read error: {e}")));
                    return;
                }
            }
        }
    });

    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(format!("no HELLO_TOKEN within {timeout:?}")),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err("daemon-stdout reader ended without a result".to_string())
        }
    }
}
