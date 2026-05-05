# Build Output — Pretty TTY Output (cli-pretty-tty)

Phase 3 → Phase 4 handoff artifact. Summarises what was built, where the
tests live, what's pinned, and what the PR description should say.

## What was built (one-line summary)

`loom session create` and `loom action ...` (and every other receipt-emitting
subcommand) now auto-detect whether stdout is a TTY: human-readable colored
multi-line output at a terminal; canonical JSON byte-for-byte unchanged when
piped. Four global flags (`--json`, `--quiet`, `--color`, `--no-color`)
override auto-detection.

## File-touch summary

**Modified (16):**
- `CHANGELOG.md` — Unreleased section with Added + Changed/breaking
- `loom-cli/src/cli_config/{cli_config.rs, mod.rs}` — `output_mode`, `stdout_color_enabled`, `stderr_color_enabled` fields; `pretty: bool` retained for back-compat
- `loom-cli/src/command_router/{command_router.rs, mod.rs}` — 4 new global flags + validate_flags re-export
- `loom-cli/src/cli_main/cli_main.rs` — pre-config flag validation + per-stream color resolution
- `loom-cli/src/error_mapper/error_mapper.rs` — `print_error_with_color` (D-20 stderr-independent)
- `loom-cli/src/output_formatter/output_formatter.rs` — `emit` / `emit_to_stdout` entry points (legacy `format_output` kept for staged migration)
- `loom-cli/src/pretty_renderer/{pretty_renderer.rs, mod.rs, interface_tests.rs}` — D-16 NO_COLOR fix + 4 new spec-compliance tests + module re-exports
- `loom-cli/src/{action,session,vault,admin,import,benchmark}_commands/*.rs` — 18 callsites migrated from `format_output(value, cfg.pretty)` to `emit_to_stdout(method, &value, cfg, None)`

**New (29):**
- `loom-cli/src/cli_config/output_mode.rs` — OutputMode enum + resolver
- `loom-cli/src/cli_config/color_choice.rs` — ColorChoice enum + per-stream resolver
- `loom-cli/src/command_router/validate_flags.rs` — mutex-flag rejector (D-7, D-31)
- `loom-cli/src/pretty_renderer/ansi.rs` — ANSI const helpers (RESET, BOLD, DIM, RED, GREEN, YELLOW, CYAN + paint + combine)
- `loom-cli/src/pretty_renderer/redact.rs` — recursive sensitive-field redaction (D-29) + 8 unit tests
- `loom-cli/src/pretty_renderer/curated/mod.rs` — registry + dispatcher + tail block (D-21..D-24) + rate-limited warning (D-32) + 5 unit tests
- `loom-cli/src/pretty_renderer/curated/{plural,session_create,session_inspect,session_list,session_close,session_abort,session_replay,session_diff,session_validate,session_export,web_navigate,web_generic_action,web_evaluate,vault_add,vault_list,vault_grant_revoke,gc,doctor,import_playwright,benchmark}.rs` — 20 files (19 renderers + 1 plural helper)
- `loom-cli/tests/integration_tty_byte_exact.rs` — 6 AC-TTY-02 byte-exact regression tests
- `loom-cli/tests/integration_tty_pretty_golden.rs` — 5 happy-path golden tests (D-34)
- `loom-cli/tests/integration_tty_flags.rs` — 17 flag precedence + color env tests (AC-TTY-03/04)
- `loom-cli/tests/integration_quiet_ids.rs` — 12 per-command --quiet identity tests
- `loom-cli/tests/integration_tail_determinism.rs` — 2 tail-order determinism tests (D-24, D-33)
- `loom-cli/tests/integration_malformed_receipts.rs` — 8 fuzz-style malformed-receipt tests (D-23, D-33)
- `loom-cli/tests/integration_perf_regression.rs` — 1 1000-item session.list perf regression (D-33)
- `loom-cli/tests/fixtures/pretty-golden/{session_create,web_navigate,gc,doctor,session_list}_at_tty.txt` — 5 fixtures

**Net diff**: ~+1500 / -180 LOC (estimated; actual `git diff main --stat` reports 18 files modified, ~336 insertions, 54 deletions, plus all new files).

## Test results

**Total**: 402 tests passing, 0 failing, 6 ignored (pre-existing
daemon-spawning E2E tests, ignored at baseline).

Net new from this PR: ~129 tests (231 lib + 17 cli_integration → was;
402 total → now).

## Acceptance evidence

| AC | Tests |
|---|---|
| AC-TTY-01 (TTY default human) | `integration_tty_pretty_golden::*` (5 golden fixtures: session_create, web_navigate, gc, doctor, session_list); plus per-command rendering verified by curated/mod.rs unit tests |
| AC-TTY-02 (non-TTY byte-exact) | `integration_tty_byte_exact::*` (6 tests covering session.create, web.navigate, session.list, alphabetical canonical key order, no-ANSI-in-canonical-path, emit-doesn't-append-newline) |
| AC-TTY-03 (--json/--pretty/--quiet override) | `integration_tty_flags::*` (17: flag precedence + conflict-exits-2), `integration_quiet_ids::*` (12) |
| AC-TTY-04 (--no-color, NO_COLOR per spec) | `integration_tty_flags::ac_tty_04_*` (7 env-var coverage tests), `pretty_renderer::interface_tests::no_color_*` (3 D-16 spec correctness tests), `cli_config::color_choice::tests::*` (8 per-stream resolver tests) |

## Quality gates

- ✅ `cargo check -p loom-cli` clean
- ✅ `cargo test -p loom-cli` 402 passing / 0 failing
- ✅ `cargo clippy -p loom-cli` no NEW lints introduced (4 pre-existing baseline clippy lints in workspace are unrelated to this PR: module_inception, derivable_impls in error_mapper, doc_lazy_continuation in action_commands, manual_map in error_mapper)
- ✅ Traceability: 100% (16/16 plan items mapped to code + tests)
- ⏳ Anti-AI review: pending (running on 300KB diff)

## Pinned behaviour (regressions caught by tests)

- AC-TTY-02 byte-exact non-TTY JSON: `serde_jcs::to_string(value) + "\n"`
- Canonical JSON sorts keys alphabetically (RFC 8785 / serde_jcs)
- `--json --pretty` together → exit 2 with Usage error
- `--color X --no-color` together → exit 2 with Usage error
- `NO_COLOR=""` does NOT disable color (D-16 / no-color.org spec)
- `CLICOLOR_FORCE=1` overrides pipe → forces color
- `TERM=dumb` disables color in `auto` mode
- `--quiet` for `loom session list` empty array → empty stdout (no header, no message)
- `--quiet` for unknown method → empty stdout
- Sensitive field names matching `(?i)(token|secret|password|api_?key|bearer|credential|cookie|jwt|session_?key|access_?token|refresh_?token|private_?key|signing_?key|client_?secret|oauth)` redacted recursively in pretty paths only — NEVER in canonical JSON path
- Tail block ordering deterministic across process invocations (no HashMap iteration leak)
- Curated renderer Err → fallback + stderr warning rate-limited to once per method per process

## PR description (draft)

```
feat(loom-cli): pretty TTY output with --json/--pretty/--quiet (AC-TTY-01..04)

## Summary

`loom session create` and `loom action ...` (and every other receipt-emitting
subcommand) now emit human-readable colored multi-line output at a terminal,
and canonical JSON (byte-for-byte unchanged) when piped. Four new global flags
(`--json`, `--quiet`, `--color`, `--no-color`) override auto-detection.

## Acceptance criteria

- [x] AC-TTY-01 — TTY default is human-readable; session create shows
  `session=<id> created`; action shows status / final_url / action_hash /
  console_count / network_summary in colored multi-line format.
- [x] AC-TTY-02 — Non-TTY JSON output is byte-for-byte identical to today;
  regression-pinned by `integration_tty_byte_exact.rs`.
- [x] AC-TTY-03 — `--json` / `--pretty` override auto-detection; `--quiet`
  prints only the canonical resource id (or errors). `--json --pretty`
  conflict → exit 2.
- [x] AC-TTY-04 — `--no-color` and `NO_COLOR` honoured per no-color.org spec
  (empty `NO_COLOR` does NOT disable; non-empty does). `--color=auto|always|never`
  + `CLICOLOR_FORCE` / `CLICOLOR=0` / `TERM=dumb` env conventions.

Spec / decisions: `specs/2026-05-04-cli-pretty-tty/` (28 locked decisions, 5
council reviewers, 100% plan→code→test traceability).

## Test plan

- [x] `cargo test -p loom-cli` — 402 passing / 0 failing
- [x] Golden fixtures in `loom-cli/tests/fixtures/pretty-golden/` for the 5
  happy-path layouts; refresh with `UPDATE_GOLDEN=1 cargo test -p loom-cli
  --test integration_tty_pretty_golden`
- [x] Manual TTY verification: `cargo run -p loom-cli -- --help` shows the new
  flag block; pipe `cargo run ... session list | cat` to confirm canonical
  JSON; `--color=always | cat` to confirm forced color.

## Breaking change

`--pretty` previously emitted indented JSON (via `serde_json::to_string_pretty`).
It now emits the human-readable colored layout. Scripts depending on indented
JSON should switch to `--json` (canonical single-line, machine-parseable).
Documented in `CHANGELOG.md` under "Changed — breaking".
```

## Status

Phase 3 complete pending anti-AI review. Phase 4 ships PR draft above.
