//! Mock harness for `loom-mcp`. Deterministic stdio-transport stubs +
//! canned tool/resource catalog.

use loom_shared::error_format::{LoomError, LoomErrorCode};
use serde_json::{json, Value};

pub struct MockMcpDispatcher;

impl MockMcpDispatcher {
    pub fn list_tools() -> Value {
        json!({
            "tools": [
                {"name": "loom.session.create"},
                {"name": "loom.session.close"},
                {"name": "loom.action.dispatch"},
            ]
        })
    }

    pub fn unknown_tool(name: &str) -> LoomError {
        LoomError::new(
            LoomErrorCode::InvalidArgument,
            format!("mock mcp: unknown tool '{name}'"),
        )
    }
}

pub struct MockToolCache;

impl MockToolCache {
    pub fn cached_count() -> usize {
        3
    }
}
