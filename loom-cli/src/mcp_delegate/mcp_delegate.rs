// Re-export of the locked Phase 5.3 interface. DO NOT EDIT here.
// Edit `systems/loom-cli/modules/McpDelegate/interfaces.rs` instead.
// McpDelegate — `loom mcp serve` delegation to loom-mcp.
//
// # Contract semantics
// - **SR-CLI-02 / AC-PROTO-03.1.** Calls
//   `loom-mcp::McpMain::run_stdio()` directly. The entire MCP loop
//   runs inside the linked-in `loom-mcp` crate; `loom-cli` does NO
//   MCP framing.
// - **`mcp-rs` banned.** `cargo deny` forbids the `mcp-rs` crate in
//   `loom-cli/Cargo.toml`. Re-implementing MCP framing inside
//   loom-cli is structurally impossible.
// - **Pass-through exit code.** Whatever exit code loom-mcp returns
//   propagates through `ErrorMapper::map_exit_code`.

use crate::CliError;

/// Run the MCP delegate. Calls `loom-mcp::McpMain::run_stdio()` and
/// awaits its termination. Returns when the parent process closes
/// stdin or sends SIGINT/SIGTERM.
///
/// loom-mcp is not yet a direct dep of loom-cli (Phase 5.4 scaffold);
/// the delegate path is wired up in Phase 6.
pub async fn run() -> Result<(), CliError> {
    Err(CliError::Internal(
        "McpDelegate: loom-mcp linkage is wired in Phase 6 — run `loom-mcp serve` directly".to_string(),
    ))
}

/// Compile-time documentation: the FQ name of the loom-mcp entry
/// point. Used by interface tests + the cargo-deny audit.
pub const LOOM_MCP_ENTRY: &str = "loom_mcp::McpMain::run_stdio";

/// The cargo-deny ban string — `interface_tests` asserts this exists
/// in the deny list.
pub const BANNED_CRATE: &str = "mcp-rs";
