// D-34 happy-path golden fixtures (5 commands). Color is disabled to
// keep fixtures readable without ANSI escapes. Edge cases (empty list,
// redaction, tail block ordering, --quiet IDs, flag conflicts) are
// covered by unit + byte-shape integration tests, NOT golden fixtures —
// keeps fixture maintenance proportional to the cost of UI tweaks.
//
// Update fixtures when intentional UI changes happen:
//   UPDATE_GOLDEN=1 cargo test -p loom-cli --test integration_tty_pretty_golden

use loom_cli::cli_config::cli_config::compiled_defaults;
use loom_cli::cli_config::output_mode::OutputMode;
use loom_cli::output_formatter::emit;
use serde_json::json;
use std::path::PathBuf;

fn cfg_pretty_no_color() -> loom_cli::cli_config::CliConfig {
    let mut c = compiled_defaults();
    c.output_mode = OutputMode::PrettyCurated;
    c.stdout_color_enabled = false;
    c
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("pretty-golden")
        .join(name)
}

fn assert_golden(name: &str, actual: &str) {
    let path = fixture_path(name);
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(&path, actual).expect("write fixture");
        eprintln!("UPDATED: {}", path.display());
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {}\nrun UPDATE_GOLDEN=1 to create", path.display(), e));
    assert_eq!(
        actual, expected,
        "golden mismatch for {}; run UPDATE_GOLDEN=1 to refresh if intentional",
        name
    );
}

#[test]
fn golden_session_create() {
    let v = json!({"session_id": "01J9ABC"});
    let out = emit("session.create", &v, &cfg_pretty_no_color(), None).unwrap();
    assert_golden("session_create_at_tty.txt", &out);
}

#[test]
fn golden_web_navigate() {
    let v = json!({
        "status": "ok",
        "final_url": "https://example.test/landing",
        "action_hash": "deadbeefcafe",
        "console_count": 3,
        "network_summary": {"total_count": 7, "total_bytes": 12345, "error_count": 0},
    });
    let out = emit("web.navigate", &v, &cfg_pretty_no_color(), None).unwrap();
    assert_golden("web_navigate_at_tty.txt", &out);
}

#[test]
fn golden_gc() {
    let v = json!({"deleted_count": 42, "freed_bytes": 1048576, "status": "ok"});
    let out = emit("gc.run", &v, &cfg_pretty_no_color(), None).unwrap();
    assert_golden("gc_at_tty.txt", &out);
}

#[test]
fn golden_doctor() {
    let v = json!({
        "status": "ok",
        "checks": [
            {"name": "daemon_running", "status": "ok"},
            {"name": "chromium_present", "status": "ok"},
            {"name": "schemas_loaded", "status": "ok"},
        ]
    });
    let out = emit("doctor", &v, &cfg_pretty_no_color(), None).unwrap();
    assert_golden("doctor_at_tty.txt", &out);
}

#[test]
fn golden_session_list_with_rows() {
    let v = json!({
        "sessions": [
            {"session_id": "01J9ABC0001", "status": "active", "created_at": "2026-05-04T10:00:00Z"},
            {"session_id": "01J9ABC0002", "status": "closed", "created_at": "2026-05-04T11:00:00Z"},
        ]
    });
    let out = emit("session.list", &v, &cfg_pretty_no_color(), None).unwrap();
    assert_golden("session_list_at_tty.txt", &out);
}
