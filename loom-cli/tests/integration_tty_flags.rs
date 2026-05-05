// Flag precedence, --json/--pretty conflict,
// --color modes, NO_COLOR / CLICOLOR / TERM=dumb env conventions.

use loom_cli::cli_config::cli_config::compiled_defaults;
use loom_cli::cli_config::color_choice::{resolve_color, ColorChoice};
use loom_cli::cli_config::output_mode::OutputMode;
use loom_cli::output_formatter::emit;
use serde_json::json;
use std::sync::Mutex;

// Env-var modifications must be serialised across tests (tests share a
// single process environment).
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn clear_color_env() {
    std::env::remove_var("NO_COLOR");
    std::env::remove_var("CLICOLOR");
    std::env::remove_var("CLICOLOR_FORCE");
    std::env::remove_var("TERM");
}

// --- mode resolution ---

#[test]
fn ac_tty_03_quiet_beats_json_and_pretty() {
    assert_eq!(
        OutputMode::resolve(true, true, true, true),
        OutputMode::Quiet
    );
}

#[test]
fn ac_tty_03_json_beats_pretty_and_auto() {
    assert_eq!(
        OutputMode::resolve(false, true, true, true),
        OutputMode::Json
    );
}

#[test]
fn ac_tty_03_pretty_overrides_pipe() {
    // --pretty into a pipe (stdout_is_terminal=false) → still pretty.
    assert_eq!(
        OutputMode::resolve(false, false, true, false),
        OutputMode::PrettyCurated
    );
}

#[test]
fn ac_tty_03_auto_pipe_yields_json() {
    assert_eq!(
        OutputMode::resolve(false, false, false, false),
        OutputMode::Json
    );
}

#[test]
fn ac_tty_03_auto_tty_yields_pretty() {
    assert_eq!(
        OutputMode::resolve(false, false, false, true),
        OutputMode::PrettyCurated
    );
}

// --- conflict (--json --pretty) ---

#[test]
fn ac_tty_03_json_and_pretty_conflict_exits_2() {
    let argv: Vec<String> = ["loom", "--json", "--pretty", "session", "list"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let code = loom_cli::cli_main::run(argv);
    assert_eq!(code, 2, "--json --pretty must exit 2 (Usage)");
}

#[test]
fn ac_tty_03_color_and_no_color_conflict_exits_2() {
    let argv: Vec<String> = ["loom", "--color", "always", "--no-color", "session", "list"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let code = loom_cli::cli_main::run(argv);
    assert_eq!(code, 2, "--color X --no-color must exit 2 (Usage)");
}

// --- color env vars (D-22) ---

#[test]
fn ac_tty_04_no_color_non_empty_disables_at_tty() {
    let _g = ENV_LOCK.lock().unwrap();
    clear_color_env();
    std::env::set_var("NO_COLOR", "1");
    assert!(!resolve_color(ColorChoice::Auto, true));
    clear_color_env();
}

#[test]
fn ac_tty_04_no_color_empty_does_not_disable() {
    let _g = ENV_LOCK.lock().unwrap();
    clear_color_env();
    std::env::set_var("NO_COLOR", "");
    // D-16 spec correctness — empty NO_COLOR is NOT a disable signal.
    assert!(resolve_color(ColorChoice::Auto, true));
    clear_color_env();
}

#[test]
fn ac_tty_04_term_dumb_disables() {
    let _g = ENV_LOCK.lock().unwrap();
    clear_color_env();
    std::env::set_var("TERM", "dumb");
    assert!(!resolve_color(ColorChoice::Auto, true));
    clear_color_env();
}

#[test]
fn ac_tty_04_color_always_forces_in_pipe() {
    let _g = ENV_LOCK.lock().unwrap();
    clear_color_env();
    assert!(resolve_color(ColorChoice::Always, false));
    clear_color_env();
}

#[test]
fn ac_tty_04_color_never_disables_at_tty() {
    let _g = ENV_LOCK.lock().unwrap();
    clear_color_env();
    assert!(!resolve_color(ColorChoice::Never, true));
    clear_color_env();
}

#[test]
fn ac_tty_04_clicolor_force_overrides_pipe() {
    let _g = ENV_LOCK.lock().unwrap();
    clear_color_env();
    std::env::set_var("CLICOLOR_FORCE", "1");
    assert!(resolve_color(ColorChoice::Auto, false));
    clear_color_env();
}

#[test]
fn ac_tty_04_clicolor_zero_disables() {
    let _g = ENV_LOCK.lock().unwrap();
    clear_color_env();
    std::env::set_var("CLICOLOR", "0");
    assert!(!resolve_color(ColorChoice::Auto, true));
    clear_color_env();
}

// --- Pretty path emits ANSI; JSON path does not ---

#[test]
fn pretty_curated_emits_ansi_when_color_enabled() {
    let mut cfg = compiled_defaults();
    cfg.output_mode = OutputMode::PrettyCurated;
    cfg.stdout_color_enabled = true;
    let v = json!({"session_id": "01ABC"});
    let bytes = emit("session.create", &v, &cfg, None).unwrap();
    assert!(
        bytes.contains('\x1b'),
        "pretty path with color must emit ESC bytes; got: {bytes:?}"
    );
}

#[test]
fn pretty_curated_no_ansi_when_color_disabled() {
    let mut cfg = compiled_defaults();
    cfg.output_mode = OutputMode::PrettyCurated;
    cfg.stdout_color_enabled = false;
    let v = json!({"session_id": "01ABC"});
    let bytes = emit("session.create", &v, &cfg, None).unwrap();
    assert!(
        !bytes.contains('\x1b'),
        "pretty path with color disabled must emit no ESC bytes; got: {bytes:?}"
    );
}

#[test]
fn json_mode_never_emits_ansi() {
    let mut cfg = compiled_defaults();
    cfg.output_mode = OutputMode::Json;
    cfg.stdout_color_enabled = true; // even with color "enabled", JSON path bypasses
    let v = json!({"session_id": "01ABC", "status": "active"});
    let bytes = emit("session.create", &v, &cfg, None).unwrap();
    assert!(
        !bytes.contains('\x1b'),
        "JSON mode must never emit ESC bytes regardless of color setting"
    );
}
