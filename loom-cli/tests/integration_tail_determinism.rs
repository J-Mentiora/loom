// D-24 / D-33 — tail block ordering must be deterministic across
// process invocations. Without explicit sorting, HashMap iteration
// would leak randomised order into output bytes and break golden tests.

use loom_cli::cli_config::cli_config::compiled_defaults;
use loom_cli::cli_config::output_mode::OutputMode;
use loom_cli::output_formatter::emit;
use serde_json::json;

#[test]
fn tail_order_byte_exact_under_no_schema_loop() {
    let v = json!({
        "session_id": "01J9ABC",
        "zeta": 26,
        "alpha": 1,
        "mu": 13,
        "kappa": 11,
        "xi": 14,
        "beta": 2,
        "omicron": 15,
        "psi": 23,
    });

    let mut cfg = compiled_defaults();
    cfg.output_mode = OutputMode::PrettyCurated;
    cfg.stdout_color_enabled = false;

    let first = emit("session.create", &v, &cfg, None).unwrap();
    for _ in 0..100 {
        let again = emit("session.create", &v, &cfg, None).unwrap();
        assert_eq!(
            first, again,
            "tail order must be byte-deterministic across runs"
        );
    }
}

#[test]
fn tail_alphabetical_when_no_schema() {
    let v = json!({
        "session_id": "01J9ABC",
        "zeta": 26,
        "alpha": 1,
        "mu": 13,
    });
    let mut cfg = compiled_defaults();
    cfg.output_mode = OutputMode::PrettyCurated;
    cfg.stdout_color_enabled = false;
    let out = emit("session.create", &v, &cfg, None).unwrap();

    // session.create renderer consumes session_id; remaining tail keys
    // (alpha, mu, zeta) must appear in alphabetical order.
    let i_alpha = out.find("alpha").expect("alpha");
    let i_mu = out.find("mu").expect("mu");
    let i_zeta = out.find("zeta").expect("zeta");
    assert!(i_alpha < i_mu && i_mu < i_zeta);
}
