//! Mock harness for `loom-host`. Deterministic canned responses for
//! WasmHost dispatch + module library lookups so dependent features
//! can TDD against a stable surface.

use loom_shared::error_format::{LoomError, LoomErrorCode};

pub const MOCK_RECEIPT_HASH: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

pub struct MockWasmHost;

impl MockWasmHost {
    /// Returns a stable Ok-shaped placeholder. A future revision will
    /// widen this to a full ActionOutcome shape once compile_module is wired.
    pub fn dispatch_ok() -> &'static str {
        "mock-action-outcome"
    }

    pub fn dispatch_unsupported(surface: &str) -> LoomError {
        LoomError::new(
            LoomErrorCode::Unsupported,
            format!("mock host has no surface '{surface}'"),
        )
    }
}

pub struct MockModuleLibrary;

impl MockModuleLibrary {
    pub fn miss(name: &str) -> LoomError {
        LoomError::new(
            LoomErrorCode::StoreNotFound,
            format!("mock module library: '{name}' not loaded"),
        )
    }
}
