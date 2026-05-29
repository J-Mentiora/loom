// Native MCP tool consumption.
//
// The MCP dispatcher exposes all registered loom-* tools with names that
// follow the `loom.<segment>.<segment>` naming convention.  The LLM
// agent therefore never has to regex-scrape: every tool it receives is a
// first-class structured MCP tool call.
//
// Run: cargo test -p loom-mcp --features mock mcp_tools_behavior

use loom_mcp::tool_cache::{ToolCache, TOOL_NAME_PREFIX};

// ---------------------------------------------------------------------------
// naming convention — all tools start with "loom."
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
// mock catalog shape — all listed tools obey the convention
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

// ---------------------------------------------------------------------------
// v0.9.6 web-cookie-injection: cookie verbs surface in tools/list
// ---------------------------------------------------------------------------

#[cfg(feature = "mock")]
#[test]
fn test_mock_dispatcher_list_includes_four_cookie_verbs() {
    // Per v0.9.6 (web-cookie-injection): MCP exposes 4 cookie tools in
    // addition to the original 10 web verbs. The MockMcpDispatcher
    // catalog must include them.
    let catalog = loom_mcp::mocks::MockMcpDispatcher::list_tools();
    let tools = catalog["tools"].as_array().expect("tools must be an array");
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();

    for expected in [
        "loom.web.set_cookies",
        "loom.web.get_cookies",
        "loom.web.clear_cookies",
        "loom.web.delete_cookies",
    ] {
        assert!(
            names.contains(&expected),
            "{expected} must appear in tools/list (v0.9.6 cookie verb); got {names:?}"
        );
    }
}

#[test]
fn test_cookie_verbs_obey_loom_dot_prefix_convention() {
    // The MCP wire names for cookie verbs follow the same convention
    // as the rest of the catalog — `loom.web.<verb>`.
    for rpc_name in [
        "web.set_cookies",
        "web.get_cookies",
        "web.clear_cookies",
        "web.delete_cookies",
    ] {
        let mcp_name = ToolCache::rpc_to_mcp_name(rpc_name);
        assert!(
            mcp_name.starts_with("loom."),
            "cookie verb {rpc_name} → {mcp_name} must obey loom. prefix"
        );
        assert_eq!(ToolCache::mcp_to_rpc_name(&mcp_name), Some(rpc_name));
    }
}
