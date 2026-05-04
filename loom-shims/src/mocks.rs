//! Mock harness for `loom-shims`. Deterministic CDP responses + IPC
//! stubs without launching real Chromium.

use loom_shared::error_format::{LoomError, LoomErrorCode};

pub struct MockCdpConnection;

impl MockCdpConnection {
    pub fn connect_ok() -> &'static str {
        "mock-cdp-connected"
    }

    pub fn navigation_ok() -> &'static str {
        "mock-navigation-complete"
    }

    pub fn timeout(verb: &str) -> LoomError {
        LoomError::new(
            LoomErrorCode::ShimTimeout,
            format!("mock shim: '{verb}' timed out"),
        )
    }

    pub fn breaker_open() -> LoomError {
        LoomError::new(LoomErrorCode::ShimBreakerOpen, "mock shim: breaker open")
    }
}

pub struct MockSupervisor;

impl MockSupervisor {
    pub fn pid() -> u32 {
        424242
    }
}
