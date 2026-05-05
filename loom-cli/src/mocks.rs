//! Mock harness for `loom-cli`. Deterministic command-router stubs
//! that bypass the real RPC client.

use loom_shared::error_format::{LoomError, LoomErrorCode};

pub struct MockCommandRouter;

impl MockCommandRouter {
    /// Canonical exit code for "command parsed + executed cleanly" in
    /// the mock. Real exit codes are mapped by `ErrorMapper`.
    pub fn exit_ok() -> i32 {
        0
    }

    pub fn exit_usage() -> i32 {
        2
    }

    pub fn unknown_command(name: &str) -> LoomError {
        LoomError::new(
            LoomErrorCode::InvalidArgument,
            format!("mock cli: unknown command '{name}'"),
        )
    }
}

pub struct MockRpcClient;

impl MockRpcClient {
    pub fn ping_ok() -> &'static str {
        "pong"
    }
}
