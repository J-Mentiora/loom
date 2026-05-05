//! — Socket creation failure exits cleanly.
//!
//! Given `$LOOM_SOCKET_PATH` points at a directory that does not exist
//! OR the parent directory is not writable,
//! When the daemon starts,
//! Then it exits with a non-zero code and a structured error; no
//! partial state is left behind.

use loom_rpc::socket_server::{BindError, SocketServer, SocketServerConfig};
use std::path::PathBuf;
use std::sync::Arc;

mod common;

#[test]
fn bind_error_on_missing_parent_directory() {
    let config = SocketServerConfig {
        socket_path: PathBuf::from("/nonexistent/deeply/nested/loom.sock"),
        token_override: None,
    };
    let deps = Arc::new(common::test_handler_deps());
    let result = SocketServer::new(config, deps);

    assert!(
        result.is_err(),
        "bind must fail when parent directory is missing"
    );
    match result.unwrap_err() {
        BindError::Io { reason } => {
            assert!(
                !reason.is_empty(),
                "BindError::Io must carry a non-empty reason"
            );
        }
        BindError::PermissionDenied => { /* also acceptable */ }
        BindError::AddressInUse => {
            panic!("AddressInUse is wrong error for missing directory");
        }
    }
}

#[test]
fn bind_error_on_read_only_directory() {
    // Create a temp dir and remove write permission.
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("loom.sock");

    // Remove write bit on the directory.
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();

    let config = SocketServerConfig {
        socket_path: socket_path.clone(),
        token_override: None,
    };
    let deps = Arc::new(common::test_handler_deps());
    let result = SocketServer::new(config, deps);

    // Restore permissions so tempdir cleanup doesn't fail.
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

    // Root bypasses permission checks — skip assertion on root.
    if unsafe { libc::getuid() } == 0 {
        return;
    }

    assert!(result.is_err(), "bind must fail on non-writable directory");
    match result.unwrap_err() {
        BindError::PermissionDenied | BindError::Io { .. } => { /* both valid */ }
        BindError::AddressInUse => {
            panic!("wrong error variant for permission-denied path");
        }
    }
}
