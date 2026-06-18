//! Daemon CLI argument parsing + startup-arg struct.
//!
//! Split out of `lib.rs` (large-file refactor), unchanged: the `DaemonArgs`
//! struct (+ its `Default`), the data-root platform default, the
//! env-then-flag `parse_args` precedence resolver, and the `--help` text.
//! Consumed by `async_main` in `lib.rs`.

use loom_rpc::socket_server::socket_server::SocketServer;
use std::path::PathBuf;

// ─── Daemon startup arguments ────────────────────────────────────────────────

#[derive(Debug)]
pub(crate) struct DaemonArgs {
    pub(crate) socket_path: PathBuf,
    pub(crate) data_root: PathBuf,
    pub(crate) log_path: PathBuf,
    pub(crate) otel_enabled: bool,
    pub(crate) default_seed: u64,
    pub(crate) checkpoint_every_n: u64,
    /// Allow-list root for `web.set_input_files` (`LOOM_UPLOAD_ROOT`).
    /// `None` → file uploads fail closed (deny all).
    pub(crate) upload_root: Option<PathBuf>,
}

impl Default for DaemonArgs {
    fn default() -> Self {
        let data_root = data_root_default();
        let log_path = data_root.join("daemon.log");
        Self {
            socket_path: SocketServer::default_socket_path(),
            data_root,
            log_path,
            otel_enabled: false,
            default_seed: 0,
            checkpoint_every_n: 100,
            upload_root: None,
        }
    }
}

fn data_root_default() -> PathBuf {
    // Per the wire-spec's data-dir conventions: macOS uses ~/Library/Application Support/loom; Linux uses $XDG_DATA_HOME/loom.
    #[cfg(target_os = "macos")]
    {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("loom")
    }
    #[cfg(not(target_os = "macos"))]
    {
        dirs::data_dir()
            .or_else(|| std::env::var("XDG_DATA_HOME").ok().map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("loom")
    }
}

/// Parse CLI args. Minimal: `--socket PATH` and `--config PATH` (config
/// parsing not yet implemented — uses env vars + defaults).
pub(crate) fn parse_args(argv: &[String]) -> DaemonArgs {
    let mut args = DaemonArgs::default();

    // Override from env vars (precedence: CLI > env > config > defaults).
    if let Ok(v) = std::env::var("LOOM_SOCKET_PATH") {
        args.socket_path = PathBuf::from(v);
    }
    if let Ok(v) = std::env::var("LOOM_DATA_ROOT") {
        let root = PathBuf::from(&v);
        args.log_path = root.join("daemon.log");
        args.data_root = root;
    }
    // Track whether the log path was set EXPLICITLY (vs derived from a data
    // root): per-setting precedence means `--data-root` may supply the
    // default `<root>/daemon.log` but must never clobber an operator-pinned
    // LOOM_LOG_PATH — monitoring reads logs from where the env said.
    let mut log_path_explicit = false;
    if let Ok(v) = std::env::var("LOOM_LOG_PATH") {
        args.log_path = PathBuf::from(v);
        log_path_explicit = true;
    }
    if std::env::var("LOOM_OTEL_ENABLED").as_deref() == Ok("1") {
        args.otel_enabled = true;
    }
    // Upload allow-list root for web.set_input_files. Unset → uploads fail
    // closed (deny all). Enforced in ALL profiles, daemon-global.
    if let Ok(v) = std::env::var("LOOM_UPLOAD_ROOT") {
        if !v.is_empty() {
            args.upload_root = Some(PathBuf::from(v));
        }
    }

    // Override socket from `--socket PATH` flag.
    let mut iter = argv.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--socket" {
            if let Some(path) = iter.next() {
                args.socket_path = PathBuf::from(path);
            }
        }
        if arg == "--data-root" {
            if let Some(path) = iter.next() {
                let root = PathBuf::from(path);
                // Only DERIVE the log path when no explicit LOOM_LOG_PATH won
                // already — the flag supplies a default location, not an
                // override of an unrelated, explicitly-set setting.
                if !log_path_explicit {
                    args.log_path = root.join("daemon.log");
                }
                args.data_root = root;
            }
        }
    }

    args
}

/// `loom-daemon --help` short-circuit. Mirrors the flags `parse_args`
/// recognises so users can discover them without grepping the source.
pub(crate) fn print_daemon_help() {
    println!(
        "loom-daemon — long-lived RPC server backing the loom CLI / SDKs.\n\
         \n\
         Usually spawned by `loom serve`. Direct invocation is supported but\n\
         rare; the CLI handles socket path + lifetime management for you.\n\
         \n\
         USAGE:\n    \
             loom-daemon [OPTIONS]\n\
         \n\
         OPTIONS:\n    \
             --socket <PATH>      Override the Unix socket path.\n    \
             --data-root <PATH>   Override the data-root directory (sessions, CAS, logs).\n                          \
             daemon.log defaults under it; an explicit LOOM_LOG_PATH wins.\n    \
             -h, --help           Print this help and exit.\n    \
             -V, --version        Print version and exit.\n\
         \n\
         ENVIRONMENT:\n    \
             LOOM_SOCKET_PATH     Same as --socket.\n    \
             LOOM_DATA_ROOT       Same as --data-root.\n    \
             LOOM_LOG_PATH        Override the daemon log file path (wins over the\n                          \
             <data-root>/daemon.log default derived from --data-root).\n    \
             LOOM_OTEL_ENABLED    Set to `1` to enable OTEL exports.\n    \
             LOOM_UPLOAD_ROOT     Allow-list root for web.set_input_files. Unset → uploads fail closed.\n    \
             LOOM_MAX_CONCURRENT_SESSIONS  Hard cap on concurrent sessions (default 16).\n    \
             LOOM_SESSION_IDLE_TTL_SECS    Evict sessions idle this long (default 1800; 0 disables).\n    \
             LOOM_REAPER_SWEEP_SECS        Reaper sweep cadence (default 60).\n    \
             LOOM_REAP_KILL_GRACE_MS       SIGTERM→SIGKILL grace per orphan tree (default 2000).\n    \
             LOOM_REAP_ORPHAN_MIN_AGE_SECS Min age before an orphan dir is GC'd (default 60).\n    \
             LOOM_REAPER_ORPHAN_GC         Orphan-Chromium GC on/off (default on; set 0 to disable).\n"
    );
}
