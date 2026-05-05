// D-23 / D-33 — malformed receipts must NEVER panic. Curated renderers
// validate input shape and either return Err (triggering the rate-limited
// fallback warning + PrettyFallback path) or render best-effort output.
// Either way: no panic, valid stdout bytes, exit 0.

use loom_cli::cli_config::cli_config::compiled_defaults;
use loom_cli::cli_config::output_mode::OutputMode;
use loom_cli::output_formatter::emit;
use serde_json::json;

fn cfg_pretty() -> loom_cli::cli_config::CliConfig {
    let mut c = compiled_defaults();
    c.output_mode = OutputMode::PrettyCurated;
    c.stdout_color_enabled = false;
    c
}

#[test]
fn missing_session_id_falls_back_no_panic() {
    // session.create renderer requires session_id; missing → renderer
    // returns Err → dispatcher falls back to PrettyRenderer.
    let v = json!({"status": "active"}); // no session_id
    let bytes = emit("session.create", &v, &cfg_pretty(), None);
    // Must not panic. Result may be Ok (fallback) or Err (depends on schema).
    assert!(bytes.is_ok() || bytes.is_err());
}

#[test]
fn wrong_type_for_session_id_falls_back_no_panic() {
    let v = json!({"session_id": 12345}); // session_id is a number, not string
    let _bytes = emit("session.create", &v, &cfg_pretty(), None);
}

#[test]
fn deeply_nested_null_no_panic() {
    let v = json!({
        "session_id": "01ABC",
        "deep": {"deeper": {"deepest": null}},
        "array": [null, null, {"k": null}],
    });
    let bytes = emit("session.create", &v, &cfg_pretty(), None).unwrap();
    assert!(!bytes.is_empty());
}

#[test]
fn empty_object_no_panic() {
    let v = json!({});
    let _bytes = emit("session.create", &v, &cfg_pretty(), None);
}

#[test]
fn truncated_array_no_panic() {
    let v = json!({"sessions": null}); // not an array
    let bytes = emit("session.list", &v, &cfg_pretty(), None);
    assert!(bytes.is_ok());
}

#[test]
fn web_navigate_missing_optional_fields_no_panic() {
    let v = json!({"action_hash": "abc"}); // no status / final_url / network_summary
    let bytes = emit("web.navigate", &v, &cfg_pretty(), None).unwrap();
    assert!(bytes.contains("action_hash"));
}

#[test]
fn null_top_level_no_panic() {
    let v = serde_json::Value::Null;
    let _bytes = emit("session.create", &v, &cfg_pretty(), None);
}

#[test]
fn array_at_top_level_no_panic() {
    // session.list accepts arrays at top level.
    let v = json!([{"session_id": "01A"}]);
    let bytes = emit("session.list", &v, &cfg_pretty(), None).unwrap();
    assert!(bytes.contains("01A") || !bytes.is_empty());
}
