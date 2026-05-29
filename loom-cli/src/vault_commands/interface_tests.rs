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

// === v0.9.6 follow-up: --credential-type value validation (ad-hoc-test
// finding: prior to this fix, --credential-type "bogus" silently fell
// through to the legacy direct-injection or OAuth path rather than
// erroring upfront, which was confusing to operators).

use super::vault_commands::validate_credential_type;

#[test]
fn validate_credential_type_accepts_oauth() {
    assert_eq!(validate_credential_type("oauth").unwrap(), "oauth");
}

#[test]
fn validate_credential_type_accepts_cookie() {
    assert_eq!(validate_credential_type("cookie").unwrap(), "cookie");
}

#[test]
fn validate_credential_type_rejects_unknown_string() {
    let err = validate_credential_type("bogus").expect_err("must reject");
    assert!(err.contains("bogus"));
    assert!(err.contains("oauth"));
    assert!(err.contains("cookie"));
}

#[test]
fn validate_credential_type_rejects_empty_string() {
    let err = validate_credential_type("").expect_err("must reject");
    assert!(err.contains("accepted values: oauth, cookie"));
}

#[test]
fn validate_credential_type_is_case_sensitive_oauth() {
    // "OAuth" (capital O) must be rejected — exact-match per the
    // clap value parser. Keeps wire/CLI naming consistent.
    assert!(validate_credential_type("OAuth").is_err());
    assert!(validate_credential_type("Oauth").is_err());
}

#[test]
fn validate_credential_type_is_case_sensitive_cookie() {
    assert!(validate_credential_type("Cookie").is_err());
    assert!(validate_credential_type("COOKIE").is_err());
}

#[test]
fn validate_credential_type_rejects_substring_matches() {
    // "oauthish" must not pass because it starts with "oauth" — guard
    // against eq-vs-starts-with subtle bugs.
    assert!(validate_credential_type("oauthish").is_err());
    assert!(validate_credential_type("cookieish").is_err());
    assert!(validate_credential_type("oauth-extra").is_err());
}

// === v0.9.6 follow-up: validate_cookie_blob_json hardening from ad-hoc tests ===

#[test]
fn validate_cookie_blob_json_rejects_schema_version_as_string() {
    // JSON `"1"` should not be silently accepted as the integer 1.
    let blob = br#"{"schema_version":"1","cookies":[]}"#;
    let err = validate_cookie_blob_json(blob).expect_err("must reject");
    let CliError::Usage(reason) = err else {
        panic!("expected Usage")
    };
    assert!(reason.contains("schema_version"), "got: {reason}");
}

#[test]
fn validate_cookie_blob_json_rejects_schema_version_as_float() {
    // `1.0` is technically a JSON number but as_u64() returns None;
    // we reject because the cap is an integer contract.
    let blob = br#"{"schema_version":1.0,"cookies":[]}"#;
    let err = validate_cookie_blob_json(blob).expect_err("must reject");
    let CliError::Usage(reason) = err else {
        panic!("expected Usage")
    };
    assert!(reason.contains("schema_version"), "got: {reason}");
}

#[test]
fn validate_cookie_blob_json_rejects_schema_version_as_negative_number() {
    let blob = br#"{"schema_version":-1,"cookies":[]}"#;
    let err = validate_cookie_blob_json(blob).expect_err("must reject");
    let CliError::Usage(reason) = err else {
        panic!("expected Usage")
    };
    assert!(reason.contains("schema_version"), "got: {reason}");
}

#[test]
fn validate_cookie_blob_json_rejects_schema_version_zero() {
    let blob = br#"{"schema_version":0,"cookies":[]}"#;
    let err = validate_cookie_blob_json(blob).expect_err("must reject");
    let CliError::Usage(reason) = err else {
        panic!("expected Usage")
    };
    assert!(reason.contains("expected 1"), "got: {reason}");
}

#[test]
fn validate_cookie_blob_json_rejects_cookies_as_object_not_array() {
    let blob = br#"{"schema_version":1,"cookies":{"name":"sid"}}"#;
    let err = validate_cookie_blob_json(blob).expect_err("must reject");
    let CliError::Usage(reason) = err else {
        panic!("expected Usage")
    };
    assert!(reason.contains("must be a JSON array"), "got: {reason}");
}

#[test]
fn validate_cookie_blob_json_rejects_cookies_array_with_null_entry() {
    let blob = br#"{"schema_version":1,"cookies":[null]}"#;
    let err = validate_cookie_blob_json(blob).expect_err("must reject");
    let CliError::Usage(reason) = err else {
        panic!("expected Usage")
    };
    assert!(reason.contains("not a JSON object"), "got: {reason}");
}

#[test]
fn validate_cookie_blob_json_tolerates_extra_top_level_fields_forward_compat() {
    // Forward-compat: extra top-level fields (e.g., a future
    // `expires_at` or `provenance` field) must NOT break the
    // validator. Only the required shape is enforced.
    let blob =
        br#"{"schema_version":1,"cookies":[{"name":"sid","value":"v","domain":"x"}],"some_future_field":42,"another":"string"}"#;
    validate_cookie_blob_json(blob).expect("extra top-level fields tolerated");
}

#[test]
fn validate_cookie_blob_json_tolerates_extra_per_cookie_fields_forward_compat() {
    // Forward-compat: extra per-cookie fields must NOT break the
    // validator. Daemon-side validate_cookie_params is the
    // authoritative shape check.
    let blob = br#"{"schema_version":1,"cookies":[{"name":"sid","value":"v","domain":"x","future_attr":42,"more":true}]}"#;
    validate_cookie_blob_json(blob).expect("extra per-cookie fields tolerated");
}

#[test]
fn validate_cookie_blob_json_accepts_utf8_in_names_and_values() {
    // Non-ASCII names + values pass validation; daemon-side
    // RFC 6265 token-char check on names will further restrict.
    let blob =
        "{\"schema_version\":1,\"cookies\":[{\"name\":\"séssion\",\"value\":\"über-token-vál\",\"domain\":\"example.com\"}]}"
            .as_bytes();
    validate_cookie_blob_json(blob).expect("utf-8 tolerated at CLI layer");
}

#[test]
fn validate_cookie_blob_json_rejects_trailing_garbage() {
    let blob = b"{\"schema_version\":1,\"cookies\":[]}garbage";
    let err = validate_cookie_blob_json(blob).expect_err("must reject");
    let CliError::Usage(reason) = err else {
        panic!("expected Usage")
    };
    assert!(reason.contains("trailing"), "got: {reason}");
}

#[test]
fn validate_cookie_blob_json_accepts_100_cookies_cli_side_no_64_cap() {
    // The CLI deliberately does NOT enforce the 64-cookie cap that
    // `validate_cookie_params` enforces daemon-side. The CLI passes the
    // blob through; the cap kicks in when `web.set_cookies` is
    // ultimately called against the grant. Documented behaviour:
    // late-failure-by-design, not a bug. This test pins that the CLI
    // accepts more than 64 cookies in the blob.
    use std::fmt::Write;
    let mut s = String::from(r#"{"schema_version":1,"cookies":["#);
    for i in 0..100 {
        if i > 0 {
            s.push(',');
        }
        write!(s, r#"{{"name":"c{i}","value":"v","domain":"x.com"}}"#).unwrap();
    }
    s.push_str("]}");
    validate_cookie_blob_json(s.as_bytes()).expect("CLI accepts 100 cookies; daemon enforces cap");
}
