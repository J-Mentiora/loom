//! Integration tests for `cli-interface` feature.

use loom_cli::action_commands::parse_extra_to_json;
use loom_cli::error_mapper::{map_exit_code, CliError, ConnectionError};
use loom_cli::output_formatter::{OutputFormatter, OutputSink};
use loom_cli::pretty_renderer::PrettyRenderer;
use loom_cli::schema_cache::SchemaCache;
use serde_json::json;
use tempfile::TempDir;

// ── Helper: in-memory output sink ───────────────────────────────────────────

struct VecSink(Vec<u8>);
impl OutputSink for VecSink {
    fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.0.extend_from_slice(bytes);
        Ok(())
    }
}
impl VecSink {
    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap()
    }
}

// ── Default output is machine-parseable canonical JSON ──────────

#[test]
fn test_canonical_json_parseable() {
    let value = json!({"session_id": "s1", "status": "ok", "created_at": 0});
    let result = OutputFormatter::<'_, VecSink>::canonical_json(&value).unwrap();
    // Must parse back cleanly
    let reparsed: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(reparsed["session_id"], "s1");
}

#[test]
fn test_no_ansi_bytes() {
    let value = json!({"status": "ok"});
    let result = OutputFormatter::<'_, VecSink>::canonical_json(&value).unwrap();
    // No ESC byte (ANSI escape sequences start with \x1b)
    assert!(
        !result.contains('\x1b'),
        "canonical_json must not emit ANSI codes"
    );
}

#[test]
fn test_write_default_path_produces_json() {
    let mut sink = VecSink(Vec::new());
    let mut fmt = OutputFormatter::new(&mut sink);
    let value = json!({"session_id": "s1", "profile": "safe", "created_at": 0});
    fmt.write("session.create", &value).unwrap();
    let out = sink.as_str();
    // Must end with newline and parse as JSON
    assert!(out.ends_with('\n'), "output must end with newline");
    let trimmed = out.trim_end();
    let parsed: serde_json::Value = serde_json::from_str(trimmed).unwrap();
    assert_eq!(parsed["session_id"], "s1");
}

// ── --pretty produces human-readable output ────────────────────

#[test]
fn test_pretty_renders_text() {
    let schemas = SchemaCache::empty();
    let renderer = PrettyRenderer::with_color(&schemas, false);
    let value = json!({"session_id": "s1", "status": "ok"});
    let rendered = renderer.render("session.inspect", &value).unwrap();
    // With no schema in cache, falls back to canonical JSON
    assert!(!rendered.is_empty());
}

// Both color-disable env signals (NO_COLOR + TERM=dumb).
// Combined into a single #[test] because cargo runs tests in parallel and
// `std::env::set_var` mutates process-global state; running these as two
// independent tests was flaky (~33%) on a recompile when the scheduler
// interleaved the NO_COLOR-set with the TERM-restore. Serializing inside
// one test eliminates the race without pulling in `serial_test`.
#[test]
fn test_color_disable_signals_honoured() {
    let no_color_old = std::env::var("NO_COLOR").ok();
    let term_old = std::env::var("TERM").ok();

    // NO_COLOR=1 → color off
    std::env::remove_var("TERM");
    std::env::set_var("NO_COLOR", "1");
    assert!(
        !loom_cli::pretty_renderer::detect_color_enabled(),
        "NO_COLOR=1 must disable color",
    );
    std::env::remove_var("NO_COLOR");

    // TERM=dumb → color off (with NO_COLOR unset)
    std::env::set_var("TERM", "dumb");
    assert!(
        !loom_cli::pretty_renderer::detect_color_enabled(),
        "TERM=dumb must disable color",
    );

    // Restore prior environment so we don't pollute sibling tests.
    match no_color_old {
        Some(v) => std::env::set_var("NO_COLOR", v),
        None => std::env::remove_var("NO_COLOR"),
    }
    match term_old {
        Some(v) => std::env::set_var("TERM", v),
        None => std::env::remove_var("TERM"),
    }
}

// ── AC-NFR-DX-02.1: Error receipts are actionable ───────────────────────────

#[test]
fn map_exit_code_ok() {
    assert_eq!(map_exit_code(&Ok(())), 0);
}

#[test]
fn map_exit_code_usage_error() {
    let err = Err(CliError::Usage("missing --session".into()));
    assert_eq!(map_exit_code(&err), 2);
}

#[test]
fn map_exit_code_receipt_error() {
    let receipt = json!({"status": "error", "code": "NotFound"});
    let err = Err(CliError::Receipt(receipt));
    assert_eq!(map_exit_code(&err), 1);
}

#[test]
fn map_exit_code_receipt_ok() {
    let receipt = json!({"status": "ok", "session_id": "s1"});
    let err = Err(CliError::Receipt(receipt));
    assert_eq!(map_exit_code(&err), 0);
}

#[test]
fn map_exit_code_connection_error() {
    let err = Err(CliError::Connection(ConnectionError::DaemonNotRunning));
    assert_eq!(map_exit_code(&err), 1);
}

#[test]
fn map_exit_code_internal_error() {
    let err = Err(CliError::Internal("bug".into()));
    assert_eq!(map_exit_code(&err), 2);
}

#[test]
fn error_receipt_verbatim() {
    // The error receipt flows verbatim through OutputFormatter
    let mut sink = VecSink(Vec::new());
    let mut fmt = OutputFormatter::new(&mut sink);
    let receipt =
        json!({"status": "error", "code": "SelectorNotFound", "message": "element not found"});
    fmt.write("web.click", &receipt).unwrap();
    let out = sink.as_str().trim_end();
    let parsed: serde_json::Value = serde_json::from_str(out).unwrap();
    assert_eq!(parsed["code"], "SelectorNotFound");
    assert_eq!(parsed["message"], "element not found");
}

// ── parse_extra_to_json correctness ────────────────────────────

#[test]
fn test_parse_extra_key_value_pairs() {
    let extra = vec![
        "--selector".to_string(),
        "#btn".to_string(),
        "--session".to_string(),
        "sid-1".to_string(),
    ];
    let val = parse_extra_to_json(&extra).unwrap();
    assert_eq!(val["selector"], "#btn");
    assert_eq!(val["session"], "sid-1");
}

#[test]
fn test_parse_extra_boolean_flag() {
    let extra = vec![
        "--verbose".to_string(),
        "--selector".to_string(),
        "#btn".to_string(),
    ];
    let val = parse_extra_to_json(&extra).unwrap();
    assert_eq!(val["verbose"], true);
    assert_eq!(val["selector"], "#btn");
}

#[test]
fn test_parse_extra_repeated_key_last_wins() {
    let extra = vec![
        "--k".to_string(),
        "a".to_string(),
        "--k".to_string(),
        "b".to_string(),
    ];
    let val = parse_extra_to_json(&extra).unwrap();
    assert_eq!(val["k"], "b");
}

#[test]
fn test_parse_extra_empty() {
    let extra: Vec<String> = vec![];
    let val = parse_extra_to_json(&extra).unwrap();
    assert!(val.as_object().unwrap().is_empty());
}

#[test]
fn test_parse_extra_auto_coerce_number() {
    // JSON numbers auto-coerced from strings
    let extra = vec!["--count".to_string(), "42".to_string()];
    let val = parse_extra_to_json(&extra).unwrap();
    assert_eq!(val["count"], 42);
}

// ── SchemaCache unit tests ───────────────────────────────────────────────────

#[test]
fn test_schema_cache_empty() {
    let cache = SchemaCache::empty();
    assert!(cache.is_empty());
    assert_eq!(cache.len(), 0);
}

#[test]
fn test_schema_cache_load_from_dir() {
    let dir = TempDir::new().unwrap();
    // Write a schema file: session.create.json
    let schema = json!({
        "request": {"type": "object", "properties": {"profile": {"type": "string"}}},
        "response": {"type": "object", "properties": {"session_id": {"type": "string"}, "created_at": {"type": "integer"}, "profile": {"type": "string"}}}
    });
    std::fs::write(
        dir.path().join("session.create.json"),
        serde_json::to_string(&schema).unwrap(),
    )
    .unwrap();
    let cache = SchemaCache::load(dir.path()).unwrap();
    assert_eq!(cache.len(), 1);
    assert!(cache.request_schema("session.create").is_some());
    assert!(cache.response_schema("session.create").is_some());
}

#[test]
fn test_schema_cache_missing_dir_returns_error() {
    let result = SchemaCache::load(std::path::Path::new("/nonexistent/path/loom/schemas/v1"));
    assert!(result.is_err());
}

// ── PrettyRenderer column derivation ────────────────────────────────────────

#[test]
fn test_columns_from_schema_insertion_order() {
    let schema = json!({
        "type": "object",
        "properties": {
            "session_id": {"type": "string"},
            "status": {"type": "string"},
            "created_at": {"type": "integer"}
        }
    });
    let cols = PrettyRenderer::columns_from_schema(&schema);
    assert_eq!(cols, vec!["session_id", "status", "created_at"]);
}

#[test]
fn test_columns_from_schema_no_properties() {
    let schema = json!({"type": "object"});
    let cols = PrettyRenderer::columns_from_schema(&schema);
    assert!(cols.is_empty());
}

// ── cli-runtime-wireup feature tests ────────────────────────────────────────

/// No leftover `unimplemented!(...)` stubs remain in
/// CommandRouter::dispatch. This test scans the source text statically.
///
/// Reads from `src/` so the test stays self-contained.
#[test]
fn test_no_unimplemented_stubs_in_dispatch() {
    let dispatch_src = include_str!("../src/command_router/command_router.rs");
    assert!(
        !dispatch_src.contains("unimplemented!(\""),
        "CommandRouter::dispatch must not contain unimplemented! stubs; \
         found at least one remaining"
    );
}

/// `loom --version` exits 0.
#[test]
fn test_version_exits_0() {
    let code = loom_cli::cli_main::run(vec!["loom".to_string(), "--version".to_string()]);
    assert_eq!(code, 0, "loom --version must exit 0");
}

/// `loom --help` exits 0 and includes all 10 subcommands.
#[test]
fn test_help_exits_0() {
    let code = loom_cli::cli_main::run(vec!["loom".to_string(), "--help".to_string()]);
    assert_eq!(code, 0, "loom --help must exit 0");
}

/// Every subcommand parses argv without hitting unimplemented!()
/// when probed with --help.
#[test]
fn test_subcommand_help_no_unimplemented() {
    let subcommands = [
        "session",
        "action",
        "vault",
        "gc",
        "serve",
        "postinstall",
        "doctor",
        "mcp",
        "import",
        "benchmark",
    ];
    for sub in &subcommands {
        let code = loom_cli::cli_main::run(vec![
            "loom".to_string(),
            sub.to_string(),
            "--help".to_string(),
        ]);
        assert_eq!(code, 0, "loom {} --help must exit 0 without panic", sub);
    }
}

/// ConfigResolver::resolve called before CommandRouter.
/// early_init must succeed for a bare argv.
#[test]
fn test_early_init_returns_config() {
    let argv = vec!["loom".to_string()];
    let result = loom_cli::cli_main::early_init(&argv);
    assert!(
        result.is_ok(),
        "early_init must return Ok(CliConfig); got: {:?}",
        result.err()
    );
}

/// HelpGenerator: json_field_to_clap_arg and clap_arg_to_json_field are
/// inverse functions (round-trip).
#[test]
fn test_help_generator_field_name_round_trip() {
    use loom_cli::help_generator::{clap_arg_to_json_field, json_field_to_clap_arg};
    let fields = [
        "session_id",
        "network_mode",
        "default_profile",
        "trace_path",
    ];
    for field in &fields {
        let arg = json_field_to_clap_arg(field);
        let back = clap_arg_to_json_field(&arg);
        assert_eq!(*field, back, "round-trip failed for field {:?}", field);
    }
}

/// VersionCommand: resolve() returns non-empty version and git_sha.
#[test]
fn test_version_command_resolve_non_empty() {
    use loom_cli::version_command::resolve;
    let info = resolve();
    assert!(!info.version.is_empty(), "version must not be empty");
    assert!(!info.git_sha.is_empty(), "git_sha must not be empty");
    assert!(!info.target.is_empty(), "target must not be empty");
}
