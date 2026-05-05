// AC-US-AGT-02.1 — Native MCP tool consumption.
//
// The MCP dispatcher exposes all registered loom-* tools with names that
// follow the `loom.<segment>.<segment>` naming convention.  The LLM
// agent therefore never has to regex-scrape: every tool it receives is a
// first-class structured MCP tool call.
//
// Run: cargo test -p loom-mcp --features mock mcp_tools_behavior

use loom_mcp::tool_cache::{ToolCache, TOOL_NAME_PREFIX};

// ---------------------------------------------------------------------------
// AC-US-AGT-02.1: naming convention — all tools start with "loom."
// ---------------------------------------------------------------------------

#[test]
fn test_tool_name_prefix_is_loom_dot() {
    // The constant drives rpc_to_mcp_name; a typo here would break every
    // MCP client that pattern-matches on the prefix.
    assert_eq!(TOOL_NAME_PREFIX, "loom.");
}

#[test]
fn test_rpc_to_mcp_name_prepends_loom_dot() {
    // Verify representative RPC method names the dispatcher would translate.
    assert_eq!(
        ToolCache::rpc_to_mcp_name("session.create"),
        "loom.session.create"
    );
    assert_eq!(
        ToolCache::rpc_to_mcp_name("session.close"),
        "loom.session.close"
    );
    assert_eq!(
        ToolCache::rpc_to_mcp_name("action.dispatch"),
        "loom.action.dispatch"
    );
    // Any future verb.noun pair preserves the invariant.
    assert_eq!(
        ToolCache::rpc_to_mcp_name("action.web.navigate"),
        "loom.action.web.navigate"
    );
}

#[test]
fn test_mcp_to_rpc_name_strips_prefix() {
    // Dispatcher uses this for the reverse mapping; None signals MethodNotFound.
    assert_eq!(
        ToolCache::mcp_to_rpc_name("loom.session.create"),
        Some("session.create")
    );
    assert_eq!(
        ToolCache::mcp_to_rpc_name("loom.action.dispatch"),
        Some("action.dispatch")
    );
    // Names that don't carry the prefix must return None — never panic.
    assert_eq!(ToolCache::mcp_to_rpc_name("session.create"), None);
    assert_eq!(ToolCache::mcp_to_rpc_name("web.navigate"), None);
    assert_eq!(ToolCache::mcp_to_rpc_name(""), None);
}

// ---------------------------------------------------------------------------
// AC-US-AGT-02.1: mock catalog shape — all listed tools obey the convention
// ---------------------------------------------------------------------------

#[cfg(feature = "mock")]
#[test]
fn test_mock_dispatcher_list_has_expected_tool_names() {
    // The mock catalog must obey the same naming rule as the real dispatcher
    // so that tests relying on MockMcpDispatcher are realistic.
    let catalog = loom_mcp::mocks::MockMcpDispatcher::list_tools();
    let tools = catalog["tools"].as_array().expect("tools must be an array");

    assert!(
        !tools.is_empty(),
        "MockMcpDispatcher must expose at least one tool"
    );

    // Every name in the catalog follows the `loom.` prefix convention.
    for tool in tools {
        let name = tool["name"].as_str().expect("tool.name must be a string");
        assert!(
            name.starts_with("loom."),
            "tool '{name}' does not follow the loom.<segment> convention"
        );
    }

    // The canonical session-create tool is always present — it is the
    // entry-point verb an agent calls to begin a browser session.
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(
        names.contains(&"loom.session.create"),
        "loom.session.create must appear in the mock tool catalog; got {names:?}"
    );
}
