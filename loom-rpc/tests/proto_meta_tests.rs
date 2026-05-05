//! — Unix socket file permissions.
//!
//! Given the `loom` daemon starts,
//! When the socket file is inspected (`stat`),
//! Then the socket has mode `0600` (rw owner only) and is owned by
//! the running user.

use loom_rpc::socket_server::{SocketServer, SocketServerConfig};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use tempfile::tempdir;

mod common;

#[test]
fn socket_mode_is_0600_after_bind() {
    let dir = tempdir().unwrap();
    let socket_path = dir.path().join("loom.sock");

    let config = SocketServerConfig {
        socket_path: socket_path.clone(),
        token_override: None,
    };
    let deps = Arc::new(common::test_handler_deps());
    let server = SocketServer::new(config, deps).expect("bind must succeed in a writable tempdir");

    let meta =
        std::fs::metadata(&socket_path).expect("socket file must exist after SocketServer::new");
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "socket mode must be exactly 0600");

    // Ownership: socket must belong to the running user.
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        meta.uid(),
        unsafe { libc::getuid() },
        "socket must be owned by the running user"
    );

    drop(server);
}
