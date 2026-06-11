// Interface tests for `SocketServer`. Verifies.1 socket
// mode, default path resolution, shared-runtime
// signature, BindError categorisation.

use super::socket_server::{BindError, SocketServer, SocketServerConfig, SOCKET_MODE};
use crate::auth_middleware::auth_middleware::Token;
use crate::connection_handler::connection_handler::ConnectionHandlerDeps;
use std::path::PathBuf;
use std::sync::Arc;

#[test]
fn socket_mode_is_0600_for_ac_proto_01_1() {
    assert_eq!(SOCKET_MODE, 0o600);
}

#[test]
fn config_carries_socket_path_and_optional_token() {
    fn _ck(c: &SocketServerConfig) {
        let _: &PathBuf = &c.socket_path;
        let _: &Option<Token> = &c.token_override;
    }
    let _ = _ck;
}

#[test]
fn new_signature_takes_config_plus_handler_deps() {
    fn _ck(
        c: SocketServerConfig,
        d: Arc<ConnectionHandlerDeps>,
    ) -> Result<SocketServer, BindError> {
        SocketServer::new(c, d)
    }
    let _ = _ck;
}

#[test]
fn bind_error_distinguishes_address_in_use_perm_denied_io() {
    // design.md §4 startup error rows.
    let _ = BindError::AddressInUse;
    let _ = BindError::PermissionDenied;
    let _ = BindError::Io { reason: "x".into() };
}

#[test]
fn server_exposes_token_field_for_loom_serve_to_print() {
    // token is printed once on `loom serve` stdout.
    fn _ck(s: &SocketServer) -> Arc<Token> {
        Arc::clone(&s.token)
    }
    let _ = _ck;
}

#[test]
fn apply_permissions_signature_takes_path() {
    fn _ck(p: &std::path::Path) -> Result<(), BindError> {
        SocketServer::apply_permissions(p)
    }
    let _ = _ck;
}

#[test]
fn default_socket_path_resolves_per_platform() {
    fn _ck() -> PathBuf {
        SocketServer::default_socket_path()
    }
    let _ = _ck;
}

/// Bind-window regression: the socket must be 0600 the
/// instant bind(2) creates it — BEFORE `apply_permissions` runs — even
/// under a fully-permissive process umask, because connect() is gated
/// by the listen backlog, not accept(). Also asserts the umask guard
/// restores the caller's umask.
#[test]
fn try_bind_creates_socket_0600_before_chmod_under_permissive_umask() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("loom.sock");

    // Worst case: fully permissive umask.
    let prev = unsafe { libc::umask(0) };
    let _listener = super::try_bind(&path).expect("bind must succeed");
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    // Guard must have restored our umask (0) after try_bind returned.
    let restored = unsafe { libc::umask(prev) };

    assert_eq!(
        mode, 0o600,
        "socket must be 0600 at bind time, before any chmod"
    );
    assert_eq!(restored, 0, "umask guard must restore the caller's umask");
}

/// Fallback-path regression: when XDG_RUNTIME_DIR is
/// unset (or empty), the non-macOS default must land in a per-user
/// directory — never the pre-squattable shared `/tmp/loom.sock`.
#[test]
fn non_macos_socket_path_never_falls_back_to_shared_tmp() {
    // XDG_RUNTIME_DIR set → socket lives inside it (0700 per XDG spec).
    assert_eq!(
        super::non_macos_socket_path(
            Some(PathBuf::from("/run/user/1000")),
            Some(PathBuf::from("/home/u/.local/share")),
        ),
        PathBuf::from("/run/user/1000/loom.sock")
    );
    // Unset → per-user data dir, NOT /tmp/loom.sock.
    assert_eq!(
        super::non_macos_socket_path(None, Some(PathBuf::from("/home/u/.local/share"))),
        PathBuf::from("/home/u/.local/share/loom/loom.sock")
    );
    // Empty XDG_RUNTIME_DIR treated as unset.
    assert_eq!(
        super::non_macos_socket_path(
            Some(PathBuf::new()),
            Some(PathBuf::from("/home/u/.local/share")),
        ),
        PathBuf::from("/home/u/.local/share/loom/loom.sock")
    );
    // No data dir either (no HOME) → per-uid /tmp dir, never bare /tmp.
    let last_resort = super::non_macos_socket_path(None, None);
    assert_ne!(last_resort, PathBuf::from("/tmp/loom.sock"));
    let uid = unsafe { libc::getuid() };
    assert!(last_resort.starts_with(format!("/tmp/loom-{uid}")));
}

#[test]
fn serve_signature_takes_runtime_handle_and_shutdown_future() {
    // shared multi-threaded runtime (no fresh runtime
    // inside loom-rpc).
    fn _assert_async() {
        async fn _ck<S: std::future::Future<Output = ()> + Send + 'static>(
            s: SocketServer,
            h: tokio::runtime::Handle,
            shutdown: S,
        ) {
            s.serve(h, shutdown).await
        }
        let _ = _ck::<std::future::Pending<()>>;
    }
    let _ = _assert_async;
}
