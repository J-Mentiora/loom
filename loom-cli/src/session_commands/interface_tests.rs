// Re-export of the locked v5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-cli/modules/SessionCommands/interface_tests.rs` instead.
// Interface tests for `SessionCommands`. Verifies subcommand coverage,
// flag-name parity, and receipt pass-through shape.

use super::session_commands::{
    parse_budget_string, AbortArgs, CapturePolicyArg, CloseArgs, CreateArgs, DiffArgs, ExportArgs,
    ExportFormat, InspectArgs, ListArgs, ReplayArgs, ValidateArgs, SUBCOMMAND_RPC_MAP,
};

// === 9 session subcommands map to 9 RPC methods ===

#[test]
fn subcommand_rpc_map_has_nine_entries() {
    assert_eq!(SUBCOMMAND_RPC_MAP.len(), 9);
}

#[test]
fn subcommand_rpc_map_covers_all_session_verbs() {
    let verbs: Vec<&str> = SUBCOMMAND_RPC_MAP.iter().map(|(k, _)| *k).collect();
    for expected in [
        "create", "inspect", "list", "close", "abort", "replay", "diff", "export", "validate",
    ] {
        assert!(verbs.contains(&expected), "missing verb {expected}");
    }
}

#[test]
fn subcommand_rpc_map_uses_session_dot_prefix() {
    for (_, rpc) in SUBCOMMAND_RPC_MAP {
        assert!(
            rpc.starts_with("session."),
            "every session.* handler must call session.* RPC; got {rpc}"
        );
    }
}

// === flag names mirror RPC schema field names ===
//
// We assert struct field names match the documented JSON-Schema fields.

#[test]
fn create_args_field_names_match_schema() {
    let json = serde_json::to_value(CreateArgs {
        profile: Some("safe".into()),
        network_mode: Some("offline".into()),
        seed: Some(42),
        budget: None,
        capture_policy: None,
        no_blocklist: false,
    })
    .unwrap();
    assert!(json.get("profile").is_some());
    // serde rename for kebab→snake — the on-the-wire JSON uses snake_case.
    assert!(json.get("network_mode").is_some());
    assert!(json.get("seed").is_some());
}

// === clap rejects unknown capture-policy values with exit 2 ===

#[test]
fn capture_policy_arg_accepts_three_known_values() {
    use clap::ValueEnum;
    let m = CapturePolicyArg::from_str("minimal", false).expect("minimal");
    assert!(matches!(m, CapturePolicyArg::Minimal));
    let d = CapturePolicyArg::from_str("default", false).expect("default");
    assert!(matches!(d, CapturePolicyArg::Default));
    let f = CapturePolicyArg::from_str("full", false).expect("full");
    assert!(matches!(f, CapturePolicyArg::Full));
}

#[test]
fn clap_rejects_capture_policy_bogus_with_exit_2() {
    use clap::{CommandFactory, FromArgMatches, Parser};

    // Wrap CreateArgs in a one-off parent so we can call try_parse_from
    // on a stable argv; clap's ValueEnum yields a parse error (exit code 2)
    // for unknown values.
    #[derive(Parser, Debug)]
    struct Wrap {
        #[command(flatten)]
        inner: CreateArgs,
    }

    let err = Wrap::command()
        .try_get_matches_from(["test", "--capture-policy", "bogus"])
        .map(|m| Wrap::from_arg_matches(&m).unwrap())
        .expect_err("clap should reject bogus capture-policy");
    // clap maps invalid value parse failures to ValueValidation /
    // InvalidValue → ErrorKind::InvalidValue, which exits 2 in the binary.
    assert_eq!(err.kind(), clap::error::ErrorKind::InvalidValue);
}

#[test]
fn clap_accepts_capture_policy_minimal() {
    use clap::{CommandFactory, FromArgMatches, Parser};
    #[derive(Parser, Debug)]
    struct Wrap {
        #[command(flatten)]
        inner: CreateArgs,
    }
    let m = Wrap::command()
        .try_get_matches_from(["test", "--capture-policy", "minimal"])
        .expect("should accept minimal");
    let parsed = Wrap::from_arg_matches(&m).expect("from_arg_matches");
    assert!(matches!(
        parsed.inner.capture_policy,
        Some(CapturePolicyArg::Minimal)
    ));
}

#[test]
fn inspect_args_carries_session_id_and_at_action() {
    let a = InspectArgs {
        session_id: "01HW".into(),
        at_action: Some(3),
    };
    assert_eq!(a.session_id, "01HW");
    assert_eq!(a.at_action, Some(3));
}

#[test]
fn list_args_is_empty_struct() {
    let _ = ListArgs {};
}

#[test]
fn close_args_carries_session_id() {
    let a = CloseArgs {
        session_id: "id".into(),
    };
    assert_eq!(a.session_id, "id");
}

#[test]
fn abort_args_carries_optional_reason() {
    let a = AbortArgs {
        session_id: "id".into(),
        reason: None,
    };
    assert!(a.reason.is_none());
}

#[test]
fn replay_args_speed_default_is_realtime_string() {
    // Default lives in the clap derive; runtime default is set when
    // parsing argv. We assert the type is `String` so `Nx`, `max`,
    // `realtime` are all expressible.
    let r = ReplayArgs {
        session_id: "id".into(),
        speed: "max".into(),
    };
    assert_eq!(r.speed, "max");
}

#[test]
fn diff_args_takes_two_session_ids() {
    let d = DiffArgs {
        a: "x".into(),
        b: "y".into(),
        include_screenshots: false,
        show_dom_diffs: true,
    };
    assert_ne!(d.a, d.b);
}

#[test]
fn export_args_format_is_value_enum() {
    let e = ExportArgs {
        session_id: "id".into(),
        format: ExportFormat::Json,
        output: None,
    };
    assert_eq!(e.format.as_wire_str(), "json");
}

#[test]
fn validate_args_takes_session_id() {
    let v = ValidateArgs {
        session_id: "id".into(),
    };
    assert_eq!(v.session_id, "id");
}

// === --budget flag parsing ===

#[test]
fn parse_budget_string_network_and_wall_clock() {
    let limits = parse_budget_string("network=10MB,wall_clock=30s").unwrap();
    assert_eq!(
        limits.network_bytes,
        10 * 1024 * 1024,
        "10MB = 10485760 bytes"
    );
    assert_eq!(limits.session_walltime_ms, 30_000, "30s = 30000ms");
    // Remaining fields retain defaults.
    assert_eq!(limits.dom_nodes, 50_000);
    assert_eq!(limits.js_heap_bytes, 512 * 1024 * 1024);
    assert_eq!(limits.action_walltime_ms, 60_000);
}

#[test]
fn parse_budget_string_unknown_key_is_error() {
    assert!(parse_budget_string("unknown=10MB").is_err());
}

#[test]
fn parse_budget_string_partial_overrides_keeps_defaults() {
    let limits = parse_budget_string("wall_clock=30s").unwrap();
    assert_eq!(limits.session_walltime_ms, 30_000);
    // Unspecified fields stay at defaults.
    assert_eq!(limits.network_bytes, 50 * 1024 * 1024);
}
