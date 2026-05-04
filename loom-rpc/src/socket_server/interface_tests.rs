// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-rpc/modules/socket_server/interface_tests.rs` instead.
// Interface tests for `SocketServer`. Verifies AC-PROTO-01.1 socket
// mode, IC-RPC-04 default path resolution, BC-RPC-01 shared-runtime
// signature, BindError categorisation.

use super::socket_server::{
    BindError, SocketServer, SocketServerConfig, SOCKET_MODE,
};
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
    let _ = BindError::Io {
        reason: "x".into(),
    };
}

#[test]
fn server_exposes_token_field_for_loom_serve_to_print() {
    // IC-RPC-05: token is printed once on `loom serve` stdout.
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

#[test]
fn serve_signature_takes_runtime_handle_and_shutdown_future() {
    // BC-RPC-01: shared multi-threaded runtime (no fresh runtime
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
