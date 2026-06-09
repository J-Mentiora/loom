//! `--clock-anchor` CLI parsing (Cluster A). Type validation is clap's job:
//! any u64 epoch is a valid clock, a non-numeric value is rejected, and the flag
//! is optional (absent → None → unchanged default behavior).

use clap::Parser;
use loom_cli::session_commands::session_commands::CreateArgs;

#[derive(Parser)]
struct Wrap {
    #[command(flatten)]
    create: CreateArgs,
}

#[test]
fn clock_anchor_parses_numeric_epoch_ms() {
    let w = Wrap::try_parse_from(["x", "--clock-anchor", "1700000000000"]).unwrap();
    assert_eq!(w.create.clock_anchor, Some(1_700_000_000_000));
}

#[test]
fn clock_anchor_rejects_non_numeric() {
    assert!(
        Wrap::try_parse_from(["x", "--clock-anchor", "not-a-number"]).is_err(),
        "clap must reject a non-numeric --clock-anchor"
    );
}

#[test]
fn clock_anchor_is_optional() {
    let w = Wrap::try_parse_from(["x"]).unwrap();
    assert_eq!(w.create.clock_anchor, None);
}

#[test]
fn clock_anchor_composes_with_seed() {
    let w = Wrap::try_parse_from(["x", "--seed", "42", "--clock-anchor", "1700000000000"]).unwrap();
    assert_eq!(w.create.seed, Some(42));
    assert_eq!(w.create.clock_anchor, Some(1_700_000_000_000));
}
