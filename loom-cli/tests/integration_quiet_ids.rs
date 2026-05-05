// `--quiet` per-command identity output. Universal rule
// per D-8 / D-19: single-resource → id; list → newline-joined ids;
// no-id-commands → silent.

use loom_cli::cli_config::cli_config::compiled_defaults;
use loom_cli::cli_config::output_mode::OutputMode;
use loom_cli::output_formatter::emit;
use serde_json::json;

fn cfg_quiet() -> loom_cli::cli_config::CliConfig {
    let mut c = compiled_defaults();
    c.output_mode = OutputMode::Quiet;
    c
}

#[test]
fn quiet_session_create_prints_session_id() {
    let v = json!({"session_id": "01J9ABC", "status": "active"});
    let bytes = emit("session.create", &v, &cfg_quiet(), None).unwrap();
    assert_eq!(bytes, "01J9ABC");
}

#[test]
fn quiet_action_navigate_prints_action_hash() {
    let v = json!({"action_hash": "deadbeef", "session_id": "01J9ABC"});
    let bytes = emit("web.navigate", &v, &cfg_quiet(), None).unwrap();
    assert_eq!(bytes, "deadbeef");
}

#[test]
fn quiet_action_click_prints_action_hash() {
    let v = json!({"action_hash": "abc123", "outcome_hash": "def456"});
    let bytes = emit("web.click", &v, &cfg_quiet(), None).unwrap();
    assert_eq!(bytes, "abc123");
}

#[test]
fn quiet_session_list_joins_with_newlines() {
    let v = json!({
        "sessions": [
            {"session_id": "01A"},
            {"session_id": "01B"},
            {"session_id": "01C"},
        ]
    });
    let bytes = emit("session.list", &v, &cfg_quiet(), None).unwrap();
    assert_eq!(bytes, "01A\n01B\n01C");
}

#[test]
fn quiet_session_list_empty_prints_nothing() {
    let v = json!({"sessions": []});
    let bytes = emit("session.list", &v, &cfg_quiet(), None).unwrap();
    assert_eq!(bytes, "");
}

#[test]
fn quiet_session_close_prints_session_id() {
    let v = json!({"session_id": "01J9ABC", "status": "ok"});
    let bytes = emit("session.close", &v, &cfg_quiet(), None).unwrap();
    assert_eq!(bytes, "01J9ABC");
}

#[test]
fn quiet_session_inspect_prints_nothing() {
    // session.inspect handler projects manifest_summary; the projected
    // value has no top-level id, so --quiet is silent (D-19).
    let v = json!({"some_field": "value"});
    let bytes = emit("session.inspect", &v, &cfg_quiet(), None).unwrap();
    assert_eq!(bytes, "");
}

#[test]
fn quiet_gc_prints_nothing() {
    let v = json!({"deleted_count": 42, "freed_bytes": 1024});
    let bytes = emit("gc.run", &v, &cfg_quiet(), None).unwrap();
    assert_eq!(bytes, "");
}

#[test]
fn quiet_doctor_prints_nothing() {
    let v = json!({"status": "ok", "checks": []});
    let bytes = emit("doctor", &v, &cfg_quiet(), None).unwrap();
    assert_eq!(bytes, "");
}

#[test]
fn quiet_unknown_method_prints_nothing() {
    let v = json!({"some_field": "value"});
    let bytes = emit("unknown.method", &v, &cfg_quiet(), None).unwrap();
    assert_eq!(bytes, "");
}

#[test]
fn quiet_session_export_prints_artifact_ref() {
    let v = json!({"artifact_ref": "ref-abc-123"});
    let bytes = emit("session.export", &v, &cfg_quiet(), None).unwrap();
    assert_eq!(bytes, "ref-abc-123");
}

#[test]
fn quiet_session_diff_prints_nothing() {
    // Diff has no top-level id (D-19).
    let v = json!({"field_diffs": [], "action_count_delta": 0});
    let bytes = emit("session.diff", &v, &cfg_quiet(), None).unwrap();
    assert_eq!(bytes, "");
}
