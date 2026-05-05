// Non-TTY JSON output must remain byte-for-byte identical to canonical
// JSON. Tests run in-process via `emit()` with
// `output_mode = Json` (which is what the resolver picks when stdout
// is not a TTY).

use loom_cli::cli_config::cli_config::compiled_defaults;
use loom_cli::cli_config::output_mode::OutputMode;
use loom_cli::output_formatter::emit;
use serde_json::json;

fn cfg_json() -> loom_cli::cli_config::CliConfig {
    let mut c = compiled_defaults();
    c.output_mode = OutputMode::Json;
    c
}

#[test]
fn tty_session_create_byte_exact() {
    let v = json!({
        "session_id": "01J9ABC",
        "status": "active",
        "profile": "safe",
        "created_at_ms": 1714867200000_u64,
    });
    let bytes = emit("session.create", &v, &cfg_json(), None).unwrap();
    let canonical = serde_jcs::to_string(&v).unwrap();
    assert_eq!(bytes, canonical, "non-TTY emit must equal canonical JCS");
}

#[test]
fn tty_action_navigate_byte_exact() {
    let v = json!({
        "action_hash": "deadbeef",
        "session_id": "01J9ABC",
        "final_url": "https://example.test/",
        "status": "ok",
        "console_count": 3,
        "network_summary": {"total_count": 7, "total_bytes": 12345, "error_count": 0},
    });
    let bytes = emit("web.navigate", &v, &cfg_json(), None).unwrap();
    let canonical = serde_jcs::to_string(&v).unwrap();
    assert_eq!(bytes, canonical);
}

#[test]
fn tty_session_list_byte_exact() {
    let v = json!({
        "sessions": [
            {"session_id": "01J9A", "status": "active", "created_at": "2026-05-04T10:00:00Z"},
            {"session_id": "01J9B", "status": "closed", "created_at": "2026-05-04T11:00:00Z"},
        ]
    });
    let bytes = emit("session.list", &v, &cfg_json(), None).unwrap();
    let canonical = serde_jcs::to_string(&v).unwrap();
    assert_eq!(bytes, canonical);
}

#[test]
fn tty_canonical_orders_keys_alphabetically() {
    // Canonical JCS sorts keys alphabetically. Non-TTY bytes must remain
    // byte-for-byte identical to today; today's behaviour is canonical
    // JCS, so key order is alphabetical regardless of insertion.
    let v = json!({"zeta": 1, "alpha": 2, "mu": 3});
    let bytes = emit("session.create", &v, &cfg_json(), None).unwrap();
    let i_alpha = bytes.find("alpha").expect("alpha");
    let i_mu = bytes.find("mu").expect("mu");
    let i_zeta = bytes.find("zeta").expect("zeta");
    assert!(
        i_alpha < i_mu && i_mu < i_zeta,
        "non-TTY canonical bytes must order keys alphabetically; got: {bytes:?}"
    );
}

#[test]
fn tty_no_ansi_in_canonical_path() {
    let v = json!({"action_hash": "deadbeef", "status": "ok"});
    let bytes = emit("web.click", &v, &cfg_json(), None).unwrap();
    assert!(
        !bytes.contains('\x1b'),
        "canonical JSON output must contain no ESC bytes; got: {bytes:?}"
    );
}

#[test]
fn tty_emit_does_not_append_newline() {
    // emit() returns bytes WITHOUT a trailing newline. The trailing
    // newline is the caller's responsibility (emit_to_stdout adds one).
    // This locks the contract used by integration tests that compare
    // against `canonical + "\n"`.
    let v = json!({"x": 1});
    let bytes = emit("session.create", &v, &cfg_json(), None).unwrap();
    assert!(!bytes.ends_with('\n'), "emit() must not append newline");
}
