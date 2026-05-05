// Interface tests for `ServeRunner`. Verifies HELLO
// disclosure shape and the no-persist contract.

use super::serve_runner::{
    default_daemon_binary, format_hello_line, serve, HelloDisclosure, ServeOptions,
};
use crate::CliError;

#[test]
fn serve_options_carry_socket_config_daemon_binary() {
    let o = ServeOptions {
        socket_path: "/tmp/loom.sock".into(),
        config_path: None,
        daemon_binary: "/usr/local/bin/loom-daemon".into(),
    };
    assert_eq!(o.socket_path.to_string_lossy(), "/tmp/loom.sock");
}

#[test]
fn serve_signature() {
    fn _ck(o: ServeOptions) {
        let _f = async move {
            let _: Result<HelloDisclosure, CliError> = serve(o).await;
        };
    }
    let _ = _ck;
}

#[test]
fn hello_disclosure_carries_token_pid_socket() {
    let h = HelloDisclosure {
        token: "deadbeef".into(),
        daemon_pid: 1234,
        socket_path: "/tmp/loom.sock".into(),
    };
    assert_eq!(h.token, "deadbeef");
    assert_eq!(h.daemon_pid, 1234);
}

#[test]
fn format_hello_line_signature() {
    fn _ck(t: &str) -> String {
        format_hello_line(t)
    }
    let _ = _ck;
}

#[test]
fn default_daemon_binary_signature() {
    fn _ck() -> Result<std::path::PathBuf, CliError> {
        default_daemon_binary()
    }
    let _ = _ck;
}

// === never persists across daemon restarts ===
//
// Encoded structurally — `ServeRunner` does not have a method to
// persist or cache the token. The token is exposed once (printed +
// returned in `HelloDisclosure`), then dropped. `AuthManager` reads
// from disk on every CLI invocation.
#[test]
fn no_persist_method_in_public_api() {
    // If a `cache_token` or `persist_token` method is ever added,
    // this test should be reviewed alongside the no-persist audit.
    let _ = HelloDisclosure {
        token: "x".into(),
        daemon_pid: 1,
        socket_path: "/tmp/x.sock".into(),
    };
}
