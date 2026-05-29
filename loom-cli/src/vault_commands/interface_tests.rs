// Interface tests for `VaultCommands`. Verifies subcommand coverage
// and prompt discipline.

use super::vault_commands::{
    VaultAddArgs, VaultGrantArgs, VaultListArgs, VaultRevokeArgs, SUBCOMMAND_RPC_MAP,
};
use crate::CliError;

// === Subcommand coverage ===
#[test]
fn subcommand_rpc_map_covers_all_verbs() {
    // v0.9.4 W6 expands the surface from 4 → 8 verbs (`add-direct` is
    // the row covering `vault add --from-stdin / --from-file`; the
    // `add` row remains for the OAuth flow).
    assert_eq!(SUBCOMMAND_RPC_MAP.len(), 8);
    let verbs: Vec<&str> = SUBCOMMAND_RPC_MAP.iter().map(|(k, _)| *k).collect();
    for v in [
        "grant",
        "revoke",
        "list",
        "add",
        "add-direct",
        "delete",
        "list-labels",
        "diagnose",
    ] {
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
        provider: Some("google".into()),
        label: None,
        from_stdin: false,
        from_file: None,
        overwrite: false,
        session: None,
        yes: true,
        credential_type: "oauth".into(),
    };
    assert!(a.yes);
}

#[test]
fn vault_add_provider_is_required_positional() {
    let a = VaultAddArgs {
        provider: Some("github".into()),
        label: None,
        from_stdin: false,
        from_file: None,
        overwrite: false,
        session: None,
        yes: false,
        credential_type: "oauth".into(),
    };
    assert_eq!(a.provider.as_deref(), Some("github"));
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

// === v0.9.6 web-cookie-injection: validate_cookie_blob_json ===

use super::vault_commands::validate_cookie_blob_json;

#[test]
fn validate_cookie_blob_json_accepts_minimal_well_formed_blob() {
    let blob = br#"{"schema_version":1,"cookies":[{"name":"sid","value":"v","domain":"x"}]}"#;
    validate_cookie_blob_json(blob).expect("minimal cookie blob is valid");
}

#[test]
fn validate_cookie_blob_json_accepts_empty_cookies_array() {
    let blob = br#"{"schema_version":1,"cookies":[]}"#;
    validate_cookie_blob_json(blob).expect("empty cookie array is valid");
}

#[test]
fn validate_cookie_blob_json_rejects_malformed_json() {
    let blob = b"not actually json at all";
    let err = validate_cookie_blob_json(blob).expect_err("must reject");
    let CliError::Usage(reason) = err else {
        panic!("expected CliError::Usage")
    };
    assert!(reason.contains("invalid cookie blob JSON"), "got: {reason}");
}

#[test]
fn validate_cookie_blob_json_rejects_missing_schema_version() {
    let blob = br#"{"cookies":[{"name":"sid","value":"v"}]}"#;
    let err = validate_cookie_blob_json(blob).expect_err("must reject");
    let CliError::Usage(reason) = err else {
        panic!("expected CliError::Usage")
    };
    assert!(reason.contains("schema_version"), "got: {reason}");
}

#[test]
fn validate_cookie_blob_json_rejects_schema_version_mismatch() {
    let blob = br#"{"schema_version":2,"cookies":[]}"#;
    let err = validate_cookie_blob_json(blob).expect_err("must reject");
    let CliError::Usage(reason) = err else {
        panic!("expected CliError::Usage")
    };
    assert!(
        reason.contains("schema_version") && reason.contains("expected 1"),
        "got: {reason}"
    );
}

#[test]
fn validate_cookie_blob_json_rejects_missing_cookies_field() {
    let blob = br#"{"schema_version":1}"#;
    let err = validate_cookie_blob_json(blob).expect_err("must reject");
    let CliError::Usage(reason) = err else {
        panic!("expected CliError::Usage")
    };
    assert!(reason.contains("cookies"), "got: {reason}");
}

#[test]
fn validate_cookie_blob_json_rejects_cookies_as_non_array() {
    let blob = br#"{"schema_version":1,"cookies":"not an array"}"#;
    let err = validate_cookie_blob_json(blob).expect_err("must reject");
    let CliError::Usage(reason) = err else {
        panic!("expected CliError::Usage")
    };
    assert!(reason.contains("array"), "got: {reason}");
}

#[test]
fn validate_cookie_blob_json_rejects_entry_missing_name_field() {
    let blob = br#"{"schema_version":1,"cookies":[{"value":"v","domain":"x"}]}"#;
    let err = validate_cookie_blob_json(blob).expect_err("must reject");
    let CliError::Usage(reason) = err else {
        panic!("expected CliError::Usage")
    };
    assert!(reason.contains("name"), "got: {reason}");
}

#[test]
fn validate_cookie_blob_json_rejects_non_object_entry() {
    let blob = br#"{"schema_version":1,"cookies":["just a string"]}"#;
    let err = validate_cookie_blob_json(blob).expect_err("must reject");
    let CliError::Usage(reason) = err else {
        panic!("expected CliError::Usage")
    };
    assert!(reason.contains("not a JSON object"), "got: {reason}");
}

#[test]
fn validate_cookie_blob_json_error_locates_bad_entry_by_index() {
    let blob = br#"{"schema_version":1,"cookies":[{"name":"ok"},{"missing_name":true}]}"#;
    let err = validate_cookie_blob_json(blob).expect_err("must reject");
    let CliError::Usage(reason) = err else {
        panic!("expected CliError::Usage")
    };
    assert!(reason.contains("entry 1"), "got: {reason}");
}

#[test]
fn vault_add_args_default_credential_type_is_oauth() {
    use clap::Parser;
    #[derive(clap::Parser)]
    struct TestCli {
        #[clap(flatten)]
        add: super::vault_commands::VaultAddArgs,
    }
    let cli = TestCli::try_parse_from(["test", "--label", "L", "--from-stdin"]).expect("parse");
    assert_eq!(cli.add.credential_type, "oauth");
}

#[test]
fn vault_add_args_credential_type_cookie_parses() {
    use clap::Parser;
    #[derive(clap::Parser)]
    struct TestCli {
        #[clap(flatten)]
        add: super::vault_commands::VaultAddArgs,
    }
    let cli = TestCli::try_parse_from([
        "test",
        "--label",
        "auth_session",
        "--credential-type",
        "cookie",
        "--from-file",
        "/tmp/cookies.json",
    ])
    .expect("parse");
    assert_eq!(cli.add.credential_type, "cookie");
    assert_eq!(cli.add.label.as_deref(), Some("auth_session"));
    assert_eq!(cli.add.from_file.as_deref(), Some("/tmp/cookies.json"));
}
