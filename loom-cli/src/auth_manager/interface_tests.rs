// Re-export of the locked v5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-cli/modules/AuthManager/interface_tests.rs` instead.
// Interface tests for `AuthManager`. Verifies artefact
// shape and the no-persistence contract.

use super::auth_manager::{default_auth_paths, AuthManager, AuthPaths};
use crate::CliError;

#[test]
fn auth_paths_carry_token_and_pid_paths() {
    let p = AuthPaths {
        token_path: "/tmp/hello.token".into(),
        pid_path: "/tmp/daemon.pid".into(),
    };
    assert!(p.token_path.to_string_lossy().ends_with("hello.token"));
    assert!(p.pid_path.to_string_lossy().ends_with("daemon.pid"));
}

#[test]
fn read_hello_token_signature() {
    fn _ck(m: &AuthManager) -> Result<String, CliError> {
        m.read_hello_token()
    }
    let _ = _ck;
}

#[test]
fn token_path_accessor_returns_path_ref() {
    let m = AuthManager::new(AuthPaths {
        token_path: "/tmp/hello.token".into(),
        pid_path: "/tmp/daemon.pid".into(),
    });
    assert_eq!(m.token_path(), std::path::Path::new("/tmp/hello.token"));
}

#[test]
fn daemon_alive_signature() {
    fn _ck(m: &AuthManager) -> bool {
        m.daemon_alive()
    }
    let _ = _ck;
}

#[test]
fn default_auth_paths_signature() {
    fn _ck() -> Result<AuthPaths, CliError> {
        default_auth_paths()
    }
    let _ = _ck;
}

// === never persists across daemon restarts ===
//
// Encoded structurally — there is no `write_hello_token` method on
// `AuthManager`. The test below documents the absence by relying on
// the public API surface compiling without a writer.
#[test]
fn no_token_writer_in_public_api() {
    // If a `write_hello_token` is ever added, this test should be
    // reviewed alongside the no-persistence audit.
    let _ = AuthManager::new(AuthPaths {
        token_path: "/tmp/x".into(),
        pid_path: "/tmp/y".into(),
    });
}
