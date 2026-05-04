//! Mock harness for `loom-surfaces`. Deterministic verb outcomes for
//! host-side TDD before WASM compilation lands in Phase 6.

use loom_shared::error_format::{LoomError, LoomErrorCode};

pub const MOCK_RECEIPT_HASH: &str =
    "1111111111111111111111111111111111111111111111111111111111111111";

pub struct MockWebSurface;

impl MockWebSurface {
    pub fn navigate_ok(url: &str) -> String {
        format!("mock-navigate:{url}")
    }

    pub fn click_ok(selector: &str) -> String {
        format!("mock-click:{selector}")
    }

    pub fn unsupported(verb: &str) -> LoomError {
        LoomError::new(
            LoomErrorCode::Unsupported,
            format!("mock web-surface: verb '{verb}' not yet implemented"),
        )
    }
}
