# Progress — Pretty TTY Output (cli-pretty-tty)

## Status: Phase 3 Step 13 complete — implementation + tests done

## Codebase patterns observed

- Every module has `mod.rs` re-exporting an `interface_tests.rs` and a same-name file (e.g. `output_formatter/output_formatter.rs`). Top of each file has "Re-export of the locked Phase 5.3 interface" comment. New modules (`curated/*.rs`, `ansi.rs`, `redact.rs`, `output_mode.rs`, `color_choice.rs`, `validate_flags.rs`) follow it.
- Handlers are `async fn name(rpc: &RpcClient, cfg: &CliConfig, args: ...) -> Result<(), CliError>` — preserved.
- Error mapping unchanged; exit codes still mapped centrally in `error_mapper`.
- `cfg.pretty: bool` retained for back-compat; `cfg.output_mode: OutputMode` is the canonical resolved value.
- Tests in `tests/` are integration; `interface_tests.rs` modules are unit. New tests follow this split.

## Plan items (P3-1..P3-13)

### Foundation [P3-1, P3-1b, P3-2, P3-3]

- [x] **P3-1** `OutputMode` enum + `ColorChoice` enum + flag plumbing
  - [x] `cli_config/output_mode.rs` (new) — verified in `loom-cli/src/cli_config/output_mode.rs`
  - [x] `cli_config/color_choice.rs` (new) — verified
  - [x] `cli_config/cli_config.rs`: `output_mode`, `stdout_color_enabled`, `stderr_color_enabled` added; `pretty: bool` retained
  - [x] `command_router/command_router.rs`: `--json`, `--quiet`, `--color`, `--no-color` globals added
  - [x] `cli_main/cli_main.rs`: validate_flags + OutputMode::resolve + per-stream resolve_color
- [x] **P3-1b** `validate_flags` helper — `command_router/validate_flags.rs`
- [x] **P3-2** New stdout entry point `output::emit` + `emit_to_stdout` — verified in `output_formatter/output_formatter.rs`
- [x] **P3-3** ANSI helper + NO_COLOR fix
  - [x] `pretty_renderer/ansi.rs` (RESET, BOLD, DIM, RED, GREEN, YELLOW, CYAN + paint + combine)
  - [x] `pretty_renderer.rs::detect_color_enabled` D-16 spec fix
  - [x] interface tests covering NO_COLOR unset / set-empty / set-non-empty / TERM=dumb

### Curated renderers [P3-4, P3-5, P3-6]

- [x] **P3-4** `quiet_id` per-renderer trait method (default None) per D-19
- [x] **P3-5** Curated registry + tail block + recursive redaction
  - [x] `pretty_renderer/curated/mod.rs`: trait + registry + dispatcher + tail block + rate-limited warning (D-32)
  - [x] `pretty_renderer/redact.rs`: `redact_recursive` + expanded regex (D-29)
  - [x] D-32 warning rate limit via `OnceLock<Mutex<HashSet<String>>>`
  - [x] Tail order: schema-prop-order if available, else alphabetical (D-24) — verified by test
- [x] **P3-6** Per-command curated renderers (one file each per D-30) — 18 files in `curated/`:
  - [x] `session_create.rs`, `session_inspect.rs`, `session_list.rs` (empty-state + column widths), `session_close.rs`, `session_abort.rs`, `session_replay.rs`, `session_diff.rs` (diff line format + plural), `session_validate.rs`, `session_export.rs`
  - [x] `web_navigate.rs` (5-line layout + plural), `web_generic_action.rs` (hash-only tier), `web_evaluate.rs`
  - [x] `vault_add.rs`, `vault_list.rs` (empty-state), `vault_grant_revoke.rs`
  - [x] `gc.rs`, `doctor.rs`, `import_playwright.rs`, `benchmark.rs`
  - [x] `plural.rs` helper (D-28)

### Errors [P3-7]

- [x] **P3-7** Error path color (D-20 stderr-independent) — `error_mapper::print_error_with_color`

### Tests [P3-8..P3-11c]

- [x] **P3-8** `tests/integration_tty_byte_exact.rs` — 6 tests, AC-TTY-02 byte-exact non-TTY regression
- [x] **P3-9** `tests/integration_tty_pretty_golden.rs` — 5 happy-path golden fixtures (D-34)
  - Fixtures in `tests/fixtures/pretty-golden/` (5 .txt files)
- [x] **P3-10** `tests/integration_tty_flags.rs` — 17 tests, flag precedence + color env vars (AC-TTY-03/04)
- [x] **P3-11** `tests/integration_quiet_ids.rs` — 12 tests, per-command --quiet identity
- [x] **P3-11b** unit tests in `curated/mod.rs` (5 tests) — tail determinism, redaction, fallback warning, empty list
- [x] **P3-11c** Determinism + malformed + perf + method-registry tests:
  - [x] `tests/integration_tail_determinism.rs` — 2 tests
  - [x] `tests/integration_malformed_receipts.rs` — 8 tests
  - [x] `tests/integration_perf_regression.rs` — 1 test (1000-item session.list <100ms local / <500ms CI)

### Docs + cleanup [P3-12, P3-13]

- [x] **P3-12** CHANGELOG entry (Unreleased): Added section + Changed/breaking section. Help text on global flags includes `--quiet` per-command behaviour and color env conventions
- [x] **P3-13** All 18 receipt-emitting callsites migrated from `format_output(value, cfg.pretty)` to `emit_to_stdout(method, &value, cfg, None)`. Three carve-outs preserved verbatim: `version_command`, `serve` HELLO_TOKEN, `session export` binary bytes

## Test counts

- Baseline (pre-PR): 273 tests passing.
- Final: **402 tests passing**, 0 failing, 6 ignored (pre-existing daemon-spawning E2E tests).
- Net new: ~129 tests.

## Quality gates

- ✅ `cargo check -p loom-cli` clean.
- ✅ `cargo test -p loom-cli` 402 passing / 0 failing.
- ✅ `cargo clippy -p loom-cli` — no new lints from this PR (3 pre-existing baseline lints unrelated: module_inception in workspace, derivable_impls in `error_mapper`, doc_lazy_continuation in `action_commands`, manual_map in `error_mapper`).

## Iteration log

- **Iteration 1** (single pass): Foundation → callsite migration → tests. No iteration loops needed; no test failures except one self-test (redaction subtree behavior) which surfaced an intentional defense-in-depth property and was retargeted to test that property explicitly.

## Acceptance evidence map (final)

| AC | Tests passing |
|---|---|
| AC-TTY-01 | `integration_tty_pretty_golden::*` (5), `pretty_curated_emits_ansi_when_color_enabled`, all `quiet_*_prints_*` |
| AC-TTY-02 | `integration_tty_byte_exact::*` (6) including byte-exact + no-ANSI + alphabetical canonical key order |
| AC-TTY-03 | `integration_tty_flags::ac_tty_03_*` (5 mode resolution), `..::*_conflict_exits_2` (2 conflict cases), `integration_quiet_ids::*` (12) |
| AC-TTY-04 | `integration_tty_flags::ac_tty_04_*` (7 env-var coverage), `pretty_renderer::interface_tests::no_color_*` (3 spec correctness), `cli_config::color_choice::tests::*` (8) |
