// Interface tests for `SocketServer`. Verifies.1 socket
// mode, default path resolution, shared-runtime
// signature, BindError categorisation.

use super::socket_server::{
    BindError, SocketFileGuard, SocketServer, SocketServerConfig, SOCKET_MODE,
};
use crate::auth_middleware::auth_middleware::Token;
use crate::connection_handler::connection_handler::ConnectionHandlerDeps;
use std::path::PathBuf;
use std::sync::Arc;

/// Serializes tests that touch PROCESS-GLOBAL state via `try_bind`. Two such states:
/// the process umask (`try_bind` flips it inside a guard around bind(2), and the
/// permissive-umask test reads it back); and tracing's global per-callsite interest cache
/// (a `try_bind` stale-reclaim WARN hit first by a no-subscriber test gets cached
/// "disabled", suppressing it for the log-capture test). Both are per-process, so under
/// parallel execution (cargo's default) these tests race. nextest runs each in its own
/// process and is immune; this keeps plain `cargo test` solid.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
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

/// Acceptance (1): dropping the socket guard unlinks the bound socket file —
/// this is what fires when `SocketServer` is dropped after graceful SIGTERM
/// shutdown, so the daemon no longer leaks `loom.sock` on disk.
#[test]
fn socket_file_guard_unlinks_bound_socket_on_drop() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("loom.sock");

    let listener = super::try_bind(&path).expect("bind must succeed");
    assert!(path.exists(), "socket file must exist while bound");

    let guard = SocketFileGuard::new(path.clone());
    // Mirror `serve`'s teardown order: the listener FD closes first, then the
    // guard (a struct field of the consumed `SocketServer`) drops and unlinks.
    drop(listener);
    drop(guard);

    assert!(
        !path.exists(),
        "guard drop must remove the socket file (no SIGTERM leak)"
    );
}

/// Race guard: if a successor daemon reclaims the path and rebinds a FRESH
/// inode (in the window after our listener FD closed but before this guard
/// runs), the guard must NOT unlink the successor's live socket — it only owns
/// the exact inode it bound.
#[test]
fn socket_file_guard_skips_unlink_when_path_rebound_by_successor() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("loom.sock");

    // Daemon A binds and arms its guard (captures A's inode identity).
    let first = super::try_bind(&path).expect("bind A");
    let guard = SocketFileGuard::new(path.clone());

    // Successor B reclaims the path: unlink A's dirent, rebind a fresh inode.
    // Keep both listeners alive so A's inode is never freed/reused — B's inode
    // is guaranteed distinct.
    std::fs::remove_file(&path).unwrap();
    let second = super::try_bind(&path).expect("bind B");
    assert!(path.exists(), "B's fresh socket must exist");

    drop(guard);

    assert!(
        path.exists(),
        "guard must not unlink a successor's rebound socket (inode differs)"
    );
    drop(first);
    drop(second);
}

/// Acceptance (2), reclaim half: a stale socket left by a dead daemon
/// (file present, nothing listening) is reclaimed — `try_bind` probe-connects,
/// gets ECONNREFUSED, unlinks, and rebinds successfully.
#[test]
fn try_bind_reclaims_stale_socket_from_dead_daemon() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    use std::os::unix::net::UnixListener as StdUnixListener;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("loom.sock");

    // Leak a socket exactly as the pre-fix daemon did: bind, then drop the
    // listener WITHOUT unlinking — the file persists with no listener behind it.
    {
        let _stale = StdUnixListener::bind(&path).expect("seed stale socket");
    }
    assert!(
        path.exists(),
        "stale socket file must persist after listener drop"
    );

    let reclaimed = super::try_bind(&path).expect("try_bind must reclaim the stale socket");
    drop(reclaimed);
}

/// Acceptance (2), log half: reclaiming a stale socket emits a WARN so the
/// previously-silent reclaim is now visible to operators.
#[test]
fn try_bind_logs_warn_when_reclaiming_stale_socket() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    use std::os::unix::net::UnixListener as StdUnixListener;
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};
    use tracing::Level;
    use tracing_subscriber::layer::{Context, Layer};
    use tracing_subscriber::prelude::*;

    #[derive(Default)]
    struct MsgVisitor(String);
    impl Visit for MsgVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.0 = format!("{value:?}");
            }
        }
    }

    struct CaptureLayer(Arc<Mutex<Vec<String>>>);
    impl<S: tracing::Subscriber> Layer<S> for CaptureLayer {
        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            if *event.metadata().level() == Level::WARN {
                let mut v = MsgVisitor::default();
                event.record(&mut v);
                self.0.lock().unwrap().push(v.0);
            }
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("loom.sock");
    {
        let _stale = StdUnixListener::bind(&path).expect("seed stale socket");
    }

    let warns = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry().with(CaptureLayer(Arc::clone(&warns)));
    tracing::subscriber::with_default(subscriber, || {
        let _ = super::try_bind(&path).expect("reclaim rebind");
    });

    let warns = warns.lock().unwrap();
    assert_eq!(
        warns.len(),
        1,
        "reclaim must emit exactly one WARN, got {warns:?}"
    );
    assert!(
        warns[0].contains("stale"),
        "warn must mention the stale socket, got {:?}",
        warns[0]
    );
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
