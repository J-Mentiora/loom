// Interface tests for `CommandRouter`. Verifies clap-derive shape and
// the subcommand-coverage invariant.

use super::command_router::{Cli, Command, ImportCmd, SessionCmd, VaultCmd};
use clap::CommandFactory;

#[test]
fn cli_is_clap_parser() {
    let _cmd = Cli::command();
}

#[test]
fn pretty_flag_is_global() {
    let cmd = Cli::command();
    let pretty_arg = cmd
        .get_arguments()
        .find(|a| a.get_id() == "pretty")
        .expect("--pretty must exist");
    assert!(
        pretty_arg.is_global_set(),
        "--pretty must be global so every subcommand inherits it"
    );
}

#[test]
fn config_flag_is_global() {
    let cmd = Cli::command();
    let cfg = cmd
        .get_arguments()
        .find(|a| a.get_id() == "config")
        .expect("--config must exist");
    assert!(cfg.is_global_set(), "--config must be global");
}

// === every subcommand variant maps to a single handler ===
//
// We don't enumerate the RPC mapping here, but we
// can lock the variant set so a missing variant breaks compilation.

#[test]
fn command_variant_set_locked() {
    fn _ck(c: Command) -> &'static str {
        match c {
            Command::Session(_) => "session",
            Command::Action(_) => "action",
            Command::Vault(_) => "vault",
            Command::Gc(_) => "gc",
            Command::Serve(_) => "serve",
            Command::Postinstall(_) => "postinstall",
            Command::Doctor(_) => "doctor",
            Command::Mcp(_) => "mcp",
            Command::Import(_) => "import",
            Command::Benchmark(_) => "benchmark",
        }
    }
    let _ = _ck;
}

#[test]
fn import_subcommand_variant_set_locked() {
    fn _ck(c: ImportCmd) -> &'static str {
        match c {
            ImportCmd::Playwright(_) => "playwright",
        }
    }
    let _ = _ck;
}

#[test]
fn session_subcommand_variant_set_locked() {
    fn _ck(c: SessionCmd) -> &'static str {
        match c {
            SessionCmd::Create(_) => "create",
            SessionCmd::Inspect(_) => "inspect",
            SessionCmd::List(_) => "list",
            SessionCmd::Close(_) => "close",
            SessionCmd::Abort(_) => "abort",
            SessionCmd::Replay(_) => "replay",
            SessionCmd::Diff(_) => "diff",
            SessionCmd::Export(_) => "export",
            SessionCmd::Validate(_) => "validate",
        }
    }
    let _ = _ck;
}

#[test]
fn vault_subcommand_variant_set_locked() {
    fn _ck(c: VaultCmd) -> &'static str {
        match c {
            VaultCmd::Grant(_) => "grant",
            VaultCmd::Revoke(_) => "revoke",
            VaultCmd::List(_) => "list",
            VaultCmd::Add(_) => "add",
            VaultCmd::Delete(_) => "delete",
            VaultCmd::ListLabels(_) => "list-labels",
            VaultCmd::Diagnose(_) => "diagnose",
        }
    }
    let _ = _ck;
}
