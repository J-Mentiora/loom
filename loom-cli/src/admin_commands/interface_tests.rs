// Re-export of the locked v5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-cli/modules/AdminCommands/interface_tests.rs` instead.
// Interface tests for `AdminCommands`. Verifies the RPC-vs-local
// split and the RPC-bypass path.

use super::admin_commands::{GcArgs, McpArgs, PostinstallArgs, ServeArgs, SUBCOMMAND_MAP};

#[test]
fn admin_subcommand_map_size() {
    assert_eq!(SUBCOMMAND_MAP.len(), 6);
}

#[test]
fn gc_is_only_rpc_admin_subcommand() {
    let rpc_entries: Vec<&str> = SUBCOMMAND_MAP
        .iter()
        .filter(|(_, v)| !v.starts_with("local:"))
        .map(|(k, _)| *k)
        .collect();
    assert_eq!(rpc_entries, vec!["gc"]);
}

#[test]
fn three_local_runners_plus_mcp_plus_version() {
    let local: Vec<&str> = SUBCOMMAND_MAP
        .iter()
        .filter(|(_, v)| v.starts_with("local:"))
        .map(|(k, _)| *k)
        .collect();
    assert_eq!(local.len(), 5);
    for expected in ["serve", "postinstall", "doctor", "mcp serve", "--version"] {
        assert!(local.contains(&expected), "missing {expected}");
    }
}

#[test]
fn gc_args_optional_fields() {
    let g = GcArgs {
        ttl: None,
        store_max_bytes: None,
    };
    assert!(g.ttl.is_none());
}

#[test]
fn serve_args_carries_socket_and_config() {
    let s = ServeArgs {
        socket: Some("/tmp/x.sock".into()),
        config: None,
    };
    assert!(s.socket.is_some());
}

#[test]
fn postinstall_args_is_empty() {
    let _ = PostinstallArgs::default();
}

#[test]
fn mcp_args_is_empty() {
    let _ = McpArgs::default();
}
