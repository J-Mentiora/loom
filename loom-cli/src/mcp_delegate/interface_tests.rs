// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-cli/modules/McpDelegate/interface_tests.rs` instead.
// Interface tests for `McpDelegate`. Verifies SR-CLI-02 delegation
// shape and the cargo-deny ban on in-line MCP framing.

use super::mcp_delegate::{run, BANNED_CRATE, LOOM_MCP_ENTRY};
use crate::CliError;

#[test]
fn run_signature_takes_no_args() {
    fn _ck() {
        let _f = async {
            let _: Result<(), CliError> = run().await;
        };
    }
    let _ = _ck;
}

#[test]
fn loom_mcp_entry_points_at_run_stdio() {
    assert_eq!(LOOM_MCP_ENTRY, "loom_mcp::McpMain::run_stdio");
}

// === SR-CLI-02: in-line MCP framing inside loom-cli → KILL ===
#[test]
fn banned_crate_is_mcp_rs() {
    assert_eq!(BANNED_CRATE, "mcp-rs");
}

// === Structural: `run` does not take a transport / framer parameter ===
//
// If `run` ever takes an MCP framer, this test should be reviewed
// alongside an audit of SR-CLI-02. The signature lock above is the
// structural enforcement.
