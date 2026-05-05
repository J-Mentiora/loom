// Re-export of the locked v5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-cli/modules/VaultCommands/interface_tests.rs` instead.
// Interface tests for `VaultCommands`. Verifies subcommand coverage
// and prompt discipline.

use super::vault_commands::{
    VaultAddArgs, VaultGrantArgs, VaultListArgs, VaultRevokeArgs, SUBCOMMAND_RPC_MAP,
};

// === Subcommand coverage ===
#[test]
fn subcommand_rpc_map_covers_four_verbs() {
    assert_eq!(SUBCOMMAND_RPC_MAP.len(), 4);
    let verbs: Vec<&str> = SUBCOMMAND_RPC_MAP.iter().map(|(k, _)| *k).collect();
    for v in ["grant", "revoke", "list", "add"] {
        assert!(verbs.contains(&v), "missing verb {v}");
    }
}

#[test]
fn list_rpc_method_is_vault_list_grants_not_vault_list() {
    // The CLI verb is `list` but the RPC is `vault.list_grants`
    // to disambiguate from `vault.list_secrets` (reserved).
    assert_eq!(
        SUBCOMMAND_RPC_MAP
            .iter()
            .find(|(k, _)| *k == "list")
            .map(|(_, v)| *v),
        Some("vault.list_grants")
    );
}

// === only `vault add` may prompt; --yes opts out ===
#[test]
fn vault_add_args_have_yes_flag() {
    let a = VaultAddArgs {
        provider: "google".into(),
        label: None,
        yes: true,
    };
    assert!(a.yes);
}

#[test]
fn vault_add_provider_is_required_positional() {
    let a = VaultAddArgs {
        provider: "github".into(),
        label: None,
        yes: false,
    };
    assert_eq!(a.provider, "github");
}

#[test]
fn vault_grant_carries_session_origin_scopes_ttl() {
    let g = VaultGrantArgs {
        session: "S".into(),
        origin: "https://example.com".into(),
        scopes: "email,profile".into(),
        ttl: 3600,
        label: Some("dev".into()),
    };
    assert_eq!(g.ttl, 3600);
    assert!(g.scopes.contains(','), "scopes is comma-separated");
}

#[test]
fn vault_revoke_takes_grant_id_and_optional_reason() {
    let r = VaultRevokeArgs {
        grant: "GR-01".into(),
        reason: None,
    };
    assert_eq!(r.grant, "GR-01");
}

#[test]
fn vault_list_session_filter_optional() {
    let l = VaultListArgs { session: None };
    assert!(l.session.is_none());
}

// === --ttl accepts integer seconds OR humantime ===

use super::vault_commands::parse_ttl;

#[test]
fn parse_ttl_accepts_bare_integer_seconds() {
    assert_eq!(parse_ttl("3600").unwrap(), 3600);
    assert_eq!(parse_ttl("0").unwrap(), 0);
    assert_eq!(parse_ttl("86400").unwrap(), 86400);
}

#[test]
fn parse_ttl_accepts_humantime_durations() {
    assert_eq!(parse_ttl("1h").unwrap(), 3600);
    assert_eq!(parse_ttl("30m").unwrap(), 1800);
    assert_eq!(parse_ttl("45s").unwrap(), 45);
    assert_eq!(parse_ttl("1h30m").unwrap(), 5400);
    assert_eq!(parse_ttl("2d").unwrap(), 2 * 24 * 3600);
}

#[test]
fn parse_ttl_rejects_garbage() {
    let err = parse_ttl("bogus").expect_err("must reject 'bogus'");
    assert!(
        err.contains("invalid"),
        "error must mention 'invalid'; got: {err}"
    );
    assert!(
        err.contains("seconds") || err.contains("duration"),
        "error must hint at accepted forms; got: {err}"
    );
}

#[test]
fn parse_ttl_rejects_empty_string() {
    let err = parse_ttl("").expect_err("must reject empty");
    assert!(
        err.contains("invalid"),
        "error must mention 'invalid'; got: {err}"
    );
}
