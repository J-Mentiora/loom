# Codebase Scan (Light) — cli-pretty-tty

**Mode**: BUILD | **Scope**: `loom-cli/` crate | **Date**: 2026-05-04

## 1. Stdout call sites (21 total)

| File | Line | Shape | Command | Notes |
|---|---|---|---|---|
| `session_commands/session_commands.rs` | 325 | `println!("{}", format_output(&resp, cfg.pretty)?)` | `session create` | RPC receipt verbatim (IC-CLI-03) |
| `session_commands/session_commands.rs` | 347-349 | `println!("{}", format_output(&inspection.manifest_summary, cfg.pretty)?)` | `session inspect` | Sub-field projection |
| `session_commands/session_commands.rs` | 357 | `println!("{}", format_output(&resp, cfg.pretty)?)` | `session list` | Receipt verbatim |
| `session_commands/session_commands.rs` | 368 | `println!("{}", format_output(&resp, cfg.pretty)?)` | `session close` | Receipt verbatim (AC-CLIEXIT-04) |
| `session_commands/session_commands.rs` | 384 | `println!("{}", format_output(&resp, cfg.pretty)?)` | `session abort` | Receipt verbatim |
| `session_commands/session_commands.rs` | 413 | `println!("{}", format_output(&resp, cfg.pretty)?)` | `session replay` | Receipt verbatim |
| `session_commands/session_commands.rs` | 437 | `println!("{}", format_output(&report.diff, cfg.pretty)?)` | `session diff` | Sub-field projection (AC-CLIEXIT3-01) |
| `session_commands/session_commands.rs` | 505-508 | `stdout().lock().write_all(&bytes)` | `session export` | **Binary artifact** — DO NOT touch |
| `session_commands/session_commands.rs` | 534-538 | `println!("PASS"/"FAIL"/"  - {reason}")` | `session validate` | Hard-coded prose; can colorise but keep parseable |
| `action_commands/action_commands.rs` | 90-92 | `println!("{}", format_output(v, cfg.pretty)...)` | URL allowlist rejection | Pre-RPC error receipt path |
| `action_commands/action_commands.rs` | 100-102 | `println!("{}", format_output(&resp, cfg.pretty)?)` | `action <surface>.<verb>` | Receipt verbatim (SR-CLI-03) |
| `vault_commands/...` | 102, 115, 123, 136 | `println!("{}", format_output(&resp, cfg.pretty)?)` | `vault grant/revoke/list/add` | Receipt verbatim |
| `admin_commands/...` | 69 | `println!("{}", format_output(&resp, cfg.pretty)?)` | `gc` | Receipt verbatim |
| `import_commands/...` | 69 | `println!("{}", format_output(&resp, cfg.pretty)?)` | `import playwright` | Receipt verbatim |
| `benchmark_commands/impl_benchmark.rs` | 49 | `println!("{}", format_output(&value, pretty)?)` | `benchmark` | Local action |
| `version_command.rs` | 46 | `println!("{json}")` | `--version` | **Direct `serde_json::to_string`** — bypasses formatter (SR-CLI-01 p99 ≤200ms) |
| `serve_runner.rs` | 62 | `println!("{}", format_hello_line(&token))` | `serve` | **Daemon handshake `HELLO_TOKEN=<hex>`** — DO NOT touch (machine consumer) |
| `command_router.rs` | 216 | `println!("{}", serde_json::to_string(report)...)` | `doctor` | Direct JSON (special case) |

## 2. Stderr (runtime)

- `error_mapper.rs:305` — `eprintln!("{msg}")` user-facing error display
- `pretty_renderer.rs:54-57` — schema-miss fallback warning

## 3. Tests pinning output shape

| Test | Pin |
|---|---|
| `tests/cli_integration.rs` | `test_ac_cli_04_1_canonical_json_parseable` (output is valid JSON), `test_ac_cli_04_1_no_ansi_bytes` (no ANSI in canonical), `test_ac_cli_04_2_pretty_renders_text` (pretty non-empty), `test_ac_cli_04_2_color_disable_signals_honoured` (NO_COLOR / TERM=dumb) |
| `tests/integration_action_errors.rs` | error message contains path; exit 2 on missing schemas; error names unknown method |
| `src/output_formatter/interface_tests.rs` | OutputFormatter::write contract |
| `src/pretty_renderer/interface_tests.rs` | NO_COLOR / TERM=dumb env-var contract |
| `src/command_router/interface_tests.rs` | `--pretty` is global; IC-CLI-03 coverage |
| `src/session_commands/interface_tests.rs` | `SUBCOMMAND_RPC_MAP` compile-time check |

**AC-TTY-02 (bit-for-bit identical non-TTY JSON) implication**: `cli_integration.rs` is the regression contract. Any new test must spawn `loom` as a subprocess with stdout redirected to a pipe so `IsTerminal` returns false, and assert on byte-exact canonical JSON.

## 4. Existing output-mode controls

| Control | Where | Effect |
|---|---|---|
| `NO_COLOR` env | `pretty_renderer.rs:101` | Disables ANSI in pretty |
| `TERM=dumb` | `pretty_renderer.rs:104` | Disables ANSI in pretty |
| `--pretty` global flag | `command_router.rs:46` → `cli_main.rs:87-88` → `cfg.pretty` | Switches `format_output` from canonical JSON to `serde_json::to_string_pretty` (indented JSON, NOT the schema-driven renderer despite IC-CLI-02 docs) |

**Important divergence found**: `format_output(value, true)` calls `serde_json::to_string_pretty` (indented JSON), while `OutputFormatter::write` (`with_pretty(...)`) routes through `PrettyRenderer::render`. Handlers use `format_output`, NOT `OutputFormatter::write`. So today's `--pretty` does indented JSON only; the schema-driven renderer's tabular path is wired but unreached. This matters for the migration plan.

## 5. TTY detection — green field

Zero uses of `is_terminal`, `atty`, or `std::io::IsTerminal` in the crate. Stable since Rust 1.70; workspace MSRV is 1.92 → use `std::io::IsTerminal` directly, no new dep.

## 6. Clap color (independent of data output)

clap default colorisation for help/error text is not explicitly tuned in `command_router.rs`. NO_COLOR and `--no-color` should also pass through to `clap::ColorChoice` so help output is consistent. (Workspace clap version per Cargo.lock.)

## Top-3 findings

1. **Single choke-point**: `format_output(value, pretty)` is called from 18 of 21 stdout sites. Refactoring it (and the renderer it delegates to) is the cleanest place to introduce TTY detection and the new format. The 3 outliers (`version`, `doctor`, `serve` HELLO, `session export` bytes) need explicit carve-outs.
2. **Existing `--pretty` semantics are indented JSON, not the schema renderer** — repurposing `--pretty` (per Step 3 Q1) is therefore even less risky than the contract docstrings suggest, since today's `--pretty` users are getting basic indented JSON anyway.
3. **No TTY infrastructure exists** — `IsTerminal` invocation, `--json`/`--quiet`/`--no-color` flags, and the curated per-command renderers are all greenfield.
