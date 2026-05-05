// D-33 perf regression — large session.list receipts must render in
// reasonable time. Threshold relaxed under CI=true.

use loom_cli::cli_config::cli_config::compiled_defaults;
use loom_cli::cli_config::output_mode::OutputMode;
use loom_cli::output_formatter::emit;
use serde_json::json;
use std::time::Instant;

#[test]
fn session_list_1000_items_under_threshold() {
    let mut sessions = Vec::with_capacity(1000);
    for i in 0..1000 {
        sessions.push(json!({
            "session_id": format!("01J9ABC{:04}", i),
            "status": if i % 3 == 0 { "active" } else { "closed" },
            "created_at": format!("2026-05-{:02}T{:02}:00:00Z", (i / 24) + 1, i % 24),
        }));
    }
    let v = json!({"sessions": sessions});

    let mut cfg = compiled_defaults();
    cfg.output_mode = OutputMode::PrettyCurated;
    cfg.stdout_color_enabled = false;

    let start = Instant::now();
    let bytes = emit("session.list", &v, &cfg, None).unwrap();
    let elapsed = start.elapsed();

    // Local <100ms; CI <500ms (D-33).
    let limit_ms = if std::env::var("CI").is_ok() { 500 } else { 100 };
    assert!(
        elapsed.as_millis() < limit_ms,
        "session.list 1000-item render took {}ms (limit {}ms)",
        elapsed.as_millis(),
        limit_ms
    );

    // Sanity: bytes contain at least one session row.
    assert!(bytes.contains("01J9ABC0000"));
}
