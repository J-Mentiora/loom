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
                // v0.9.6 web-cookie-injection: 4 cookie verbs surface
                // via the same MCP convention. The real dispatcher
                // resolves these from the registry on the daemon side;
                // the mock catalog mirrors them so tests can assert
                // tools/list contents without spinning up a daemon.
                {"name": "loom.web.set_cookies"},
                {"name": "loom.web.get_cookies"},
                {"name": "loom.web.clear_cookies"},
                {"name": "loom.web.delete_cookies"},
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
        7
    }
}
