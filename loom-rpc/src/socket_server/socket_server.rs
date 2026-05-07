// SocketServer — owns the `tokio::net::UnixListener` at the platform
// socket path; mode 0600; stale-socket recovery; graceful shutdown.
//
// # Contract semantics
// - **Socket path.** Default
//   `~/Library/Caches/loom/loom.sock` on macOS;
//   `$XDG_RUNTIME_DIR/loom.sock` on Linux. Overridable via config
//   file → env (`LOOM_SOCKET_PATH`) → CLI flag (`--socket`).
// - **Permissions.** Mode `0600` set via
//   `std::os::unix::fs::PermissionsExt` immediately after bind, before
//   the first `accept()`.
// - **Stale-socket recovery (design.md §4).** On bind `EADDRINUSE`,
//   probe-connect; if `ECONNREFUSED`, `unlink()` the socket file and
//   retry bind once. Second failure is fatal — daemon refuses to
//   start.
// - **Shared tokio runtime .** `serve(handle)` borrows the
//   daemon's `tokio::Handle`; per-connection tasks spawn onto this
//   handle. No fresh runtime is constructed inside loom-rpc.
// - **Graceful shutdown.** `shutdown_signal` future resolves when the
//   daemon receives SIGINT/SIGTERM; in-flight connections drain
//   before the listener releases the FD.

use crate::auth_middleware::auth_middleware::Token;
use crate::connection_handler::connection_handler::ConnectionHandlerDeps;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::Arc;

/// Socket file mode required by.
pub const SOCKET_MODE: u32 = 0o600;

/// Configuration loaded by the daemon (precedence).
#[derive(Debug, Clone)]
pub struct SocketServerConfig {
    pub socket_path: PathBuf,
    /// Optional override for the per-startup token. `None` means the
    /// server generates one via `Token::generate()`.
    pub token_override: Option<Token>,
}

/// Errors that can occur during bind. Mapped to startup-failure exit
/// codes by the daemon harness.
#[derive(Debug)]
pub enum BindError {
    /// `EADDRINUSE` and probe-connect succeeded — another daemon is
    /// running. Fatal.
    AddressInUse,
    /// `EACCES` on bind. Operator must fix socket-dir permissions.
    PermissionDenied,
    /// Generic I/O failure during bind / chmod / unlink.
    Io { reason: String },
}

/// Public handle returned from `SocketServer::new`. Carries the
/// generated (or overridden) token for `loom serve` to print on stdout
/// per.
pub struct SocketServer {
    pub(crate) listener: UnixListener,
    pub(crate) deps: Arc<ConnectionHandlerDeps>,
    pub token: Arc<Token>,
}
