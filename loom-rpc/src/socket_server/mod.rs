//! `socket_server` — see crate root.
pub mod socket_server;
pub use socket_server::*;

#[cfg(test)]
mod interface_tests;

use crate::auth_middleware::auth_middleware::Token;
use crate::connection_handler::connection_handler::{ConnectionHandler, ConnectionHandlerDeps};
use std::fmt;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener as StdUnixListener;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::Path;
use std::sync::Arc;

impl fmt::Debug for SocketServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SocketServer").finish_non_exhaustive()
    }
}

impl SocketServer {
    /// Public associated-function form of `apply_permissions` .
    pub fn apply_permissions(path: &std::path::Path) -> Result<(), BindError> {
        apply_permissions(path)
    }

    /// Public associated-function form of `default_socket_path` .
    pub fn default_socket_path() -> std::path::PathBuf {
        default_socket_path()
    }

    /// Bind the Unix socket, set mode 0600, and return the server handle.
    /// Stale-socket recovery: on EADDRINUSE, probe-connect; if refused,
    /// unlink and retry once.
    pub fn new(
        config: SocketServerConfig,
        deps: Arc<ConnectionHandlerDeps>,
    ) -> Result<Self, BindError> {
        let token = config.token_override.unwrap_or_else(Token::generate);
        let token = Arc::new(token);
        let path = &config.socket_path;

        let listener = try_bind(path)?;
        // Arm socket-file cleanup the instant the bind succeeds: if a later step
        // in `new` (chmod / set_nonblocking) fails, the guard drops on the early
        // return and removes the partial socket; on success it rides on the
        // returned handle and unlinks `loom.sock` at graceful shutdown (#141).
        let socket_guard = SocketFileGuard::new(path.clone());
        apply_permissions(path)?;
        listener.set_nonblocking(true).map_err(|e| BindError::Io {
            reason: e.to_string(),
        })?;

        Ok(Self {
            listener,
            deps,
            token,
            _socket_guard: socket_guard,
        })
    }

    /// Accept-loop. Spawns a `ConnectionHandler` task per connection.
    /// Returns when `shutdown` resolves.
    pub async fn serve<S>(self, _handle: tokio::runtime::Handle, shutdown: S)
    where
        S: std::future::Future<Output = ()>,
    {
        let listener = match tokio::net::UnixListener::from_std(self.listener) {
            Ok(l) => l,
            Err(_) => return,
        };
        let deps = self.deps;
        let _token = self.token; // held for lifetime; token stored in auth middleware

        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown => break,
                result = listener.accept() => {
                    match result {
                        Ok((stream, _)) => {
                            let deps_c = Arc::clone(&deps);
                            tokio::spawn(async move {
                                ConnectionHandler::new(deps_c).run(stream).await;
                            });
                        }
                        Err(_) => break,
                    }
                }
            }
        }
    }
}

/// RAII guard: set the process umask, restore the previous value on drop.
struct UmaskGuard {
    prev: libc::mode_t,
}

impl UmaskGuard {
    fn set(mask: libc::mode_t) -> Self {
        // SAFETY: umask(2) is always successful, affects only this
        // process, and has no memory-safety preconditions.
        let prev = unsafe { libc::umask(mask) };
        Self { prev }
    }
}

impl Drop for UmaskGuard {
    fn drop(&mut self) {
        unsafe { libc::umask(self.prev) };
    }
}

fn try_bind(path: &Path) -> Result<StdUnixListener, BindError> {
    // Bind under a restrictive umask so the socket inode is created
    // 0600 (0777 & !0o177) from the very first instant — previously the
    // socket existed with umask-default (commonly world-connectable)
    // permissions between bind(2) and `apply_permissions`, and connect()
    // is gated by the listen backlog, not by accept(), so another local
    // user could connect during that window. `apply_permissions` still
    // runs afterwards as belt-and-braces; the unlink-and-rebind
    // recovery path below is covered by the same guard.
    let _umask = UmaskGuard::set(0o177);
    match StdUnixListener::bind(path) {
        Ok(l) => Ok(l),
        Err(e) if e.kind() == ErrorKind::AddrInUse => match StdUnixStream::connect(path) {
            Ok(_) => Err(BindError::AddressInUse),
            Err(_) => {
                tracing::warn!(
                    path = ?path,
                    "removing stale loom.sock from a previous daemon (connect refused)"
                );
                std::fs::remove_file(path).map_err(|e| BindError::Io {
                    reason: e.to_string(),
                })?;
                StdUnixListener::bind(path).map_err(map_io_err)
            }
        },
        Err(e) => Err(map_io_err(e)),
    }
}

fn apply_permissions(path: &Path) -> Result<(), BindError> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(SOCKET_MODE)).map_err(|e| {
        BindError::Io {
            reason: e.to_string(),
        }
    })
}

fn map_io_err(e: std::io::Error) -> BindError {
    match e.kind() {
        ErrorKind::PermissionDenied => BindError::PermissionDenied,
        _ => BindError::Io {
            reason: e.to_string(),
        },
    }
}

/// Default socket path per the contract /.
pub fn default_socket_path() -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    {
        dirs::cache_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
            .join("loom")
            .join("loom.sock")
    }
    #[cfg(not(target_os = "macos"))]
    {
        non_macos_socket_path(
            std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from),
            dirs::data_dir(),
        )
    }
}

/// Non-macOS resolution order: `$XDG_RUNTIME_DIR/loom.sock` when the
/// runtime dir is set and non-empty (it is per-user mode 0700 by the
/// XDG spec), else the per-user data dir
/// (`~/.local/share/loom/loom.sock`), else a per-uid `/tmp/loom-<uid>`
/// directory. Never bare shared `/tmp`, whose predictable `loom.sock`
/// name another local user could pre-squat to deny daemon startup or
/// impersonate the daemon to a connecting client.
///
/// Compiled on every platform so the resolution order stays
/// unit-testable from macOS dev machines.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn non_macos_socket_path(
    xdg_runtime_dir: Option<std::path::PathBuf>,
    data_dir: Option<std::path::PathBuf>,
) -> std::path::PathBuf {
    if let Some(dir) = xdg_runtime_dir.filter(|d| !d.as_os_str().is_empty()) {
        return dir.join("loom.sock");
    }
    data_dir
        .map(|d| d.join("loom"))
        .unwrap_or_else(|| {
            // SAFETY: getuid(2) cannot fail.
            let uid = unsafe { libc::getuid() };
            std::path::PathBuf::from(format!("/tmp/loom-{uid}"))
        })
        .join("loom.sock")
}
