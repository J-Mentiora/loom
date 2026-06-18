// Interface tests for `SessionCommands`. Verifies subcommand coverage,
// flag-name parity, and receipt pass-through shape.

use super::session_commands::{
    parse_budget_string, parse_replay_speed, AbortArgs, CapturePolicyArg, CloseArgs, CreateArgs,
    DiffArgs, ExportArgs, ExportFormat, InspectArgs, ListArgs, ReplayArgs, ReplaySpeed,
    ValidateArgs, SUBCOMMAND_RPC_MAP,
};

// === 10 session subcommands map to 10 RPC methods ===

#[test]
fn subcommand_rpc_map_has_ten_entries() {
    assert_eq!(SUBCOMMAND_RPC_MAP.len(), 10);
}

#[test]
fn subcommand_rpc_map_covers_all_session_verbs() {
    let verbs: Vec<&str> = SUBCOMMAND_RPC_MAP.iter().map(|(k, _)| *k).collect();
    for expected in [
        "create", "inspect", "list", "close", "abort", "replay", "diff", "export", "validate",
        "reap",
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
        no_determinism: false,
        record_screencast: false,
        clock_anchor: Some(1_700_000_000_000),
    })
    .unwrap();
    assert!(json.get("profile").is_some());
    // serde rename for kebab→snake — the on-the-wire JSON uses snake_case.
    assert!(json.get("network_mode").is_some());
    assert!(json.get("seed").is_some());
    assert!(json.get("clock_anchor").is_some());
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

// === --clock-anchor: clap validates the u64 epoch for free (T5) ===

#[test]
fn clap_rejects_non_numeric_clock_anchor() {
    use clap::{CommandFactory, FromArgMatches, Parser};
    #[derive(Parser, Debug)]
    struct Wrap {
        #[command(flatten)]
        inner: CreateArgs,
    }
    // `Option<u64>` parsing rejects a non-numeric value at the clap boundary —
    // no hand-rolled validation needed. Any valid u64 is a semantically valid
    // epoch, so there is no range check / recovery path.
    let err = Wrap::command()
        .try_get_matches_from(["test", "--clock-anchor", "not-a-number"])
        .map(|m| Wrap::from_arg_matches(&m).unwrap())
        .expect_err("clap should reject non-numeric --clock-anchor");
    assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
}

#[test]
fn clap_accepts_numeric_clock_anchor() {
    use clap::{CommandFactory, FromArgMatches, Parser};
    #[derive(Parser, Debug)]
    struct Wrap {
        #[command(flatten)]
        inner: CreateArgs,
    }
    let m = Wrap::command()
        .try_get_matches_from(["test", "--clock-anchor", "1700000000000"])
        .map(|m| Wrap::from_arg_matches(&m).unwrap())
        .expect("clap should accept a numeric epoch");
    assert_eq!(m.inner.clock_anchor, Some(1_700_000_000_000));
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

// === --speed parsing (regression: the CLI forwarded the raw string and
// the daemon's `as_f64` parse silently dropped every documented value) ===

#[test]
fn parse_replay_speed_accepts_documented_forms() {
    assert_eq!(parse_replay_speed("realtime"), Ok(ReplaySpeed::Realtime));
    assert_eq!(parse_replay_speed("max"), Ok(ReplaySpeed::Max));
    assert_eq!(parse_replay_speed("2x"), Ok(ReplaySpeed::Multiplier(2.0)));
    assert_eq!(parse_replay_speed("1.5x"), Ok(ReplaySpeed::Multiplier(1.5)));
    // Case/whitespace tolerant.
    assert_eq!(parse_replay_speed(" MAX "), Ok(ReplaySpeed::Max));
    assert_eq!(parse_replay_speed("2X"), Ok(ReplaySpeed::Multiplier(2.0)));
}

#[test]
fn parse_replay_speed_rejects_garbage_with_accepted_forms_in_message() {
    for bad in ["fast", "2", "x", "0x", "-2x", "infx", "nanx", ""] {
        let err = parse_replay_speed(bad).expect_err(&format!("speed {bad:?} must be rejected"));
        assert!(
            err.contains("max") && err.contains("realtime"),
            "error must list accepted forms; got: {err}"
        );
    }
}

#[test]
fn replay_speed_wire_form_is_numeric() {
    // The daemon parses `speed` with `as_f64`; the SDKs send numbers.
    assert_eq!(ReplaySpeed::Realtime.as_wire_f64(), 1.0);
    assert_eq!(ReplaySpeed::Multiplier(2.5).as_wire_f64(), 2.5);
    // `max` maps to the 0 = unpaced sentinel.
    assert_eq!(ReplaySpeed::Max.as_wire_f64(), 0.0);
    // And the params value built from it is a JSON number, not a string.
    let v = serde_json::json!({ "speed": ReplaySpeed::Max.as_wire_f64() });
    assert!(v["speed"].is_number(), "speed must be numeric on the wire");
}

#[test]
fn clap_rejects_bogus_speed_with_exit_2() {
    use clap::{CommandFactory, Parser};
    #[derive(Parser, Debug)]
    struct Wrap {
        #[command(flatten)]
        inner: ReplayArgs,
    }
    let err = Wrap::command()
        .try_get_matches_from(["test", "sid", "--speed", "warp9"])
        .expect_err("clap should reject bogus --speed");
    assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
}

#[test]
fn clap_speed_default_is_realtime() {
    use clap::{CommandFactory, FromArgMatches, Parser};
    #[derive(Parser, Debug)]
    struct Wrap {
        #[command(flatten)]
        inner: ReplayArgs,
    }
    let m = Wrap::command()
        .try_get_matches_from(["test", "sid"])
        .expect("no --speed should parse with default");
    let parsed = Wrap::from_arg_matches(&m).expect("from_arg_matches");
    assert_eq!(parsed.inner.speed, ReplaySpeed::Realtime);

    let m = Wrap::command()
        .try_get_matches_from(["test", "sid", "--speed", "2x"])
        .expect("--speed 2x should parse");
    let parsed = Wrap::from_arg_matches(&m).expect("from_arg_matches");
    assert_eq!(parsed.inner.speed, ReplaySpeed::Multiplier(2.0));
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
