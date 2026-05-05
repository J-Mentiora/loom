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
        apply_permissions(path)?;
        listener.set_nonblocking(true).map_err(|e| BindError::Io {
            reason: e.to_string(),
        })?;

        Ok(Self {
            listener,
            deps,
            token,
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

fn try_bind(path: &Path) -> Result<StdUnixListener, BindError> {
    match StdUnixListener::bind(path) {
        Ok(l) => Ok(l),
        Err(e) if e.kind() == ErrorKind::AddrInUse => match StdUnixStream::connect(path) {
            Ok(_) => Err(BindError::AddressInUse),
            Err(_) => {
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
        std::env::var("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
            .join("loom.sock")
    }
}
