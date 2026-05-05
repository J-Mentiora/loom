# Phase 3 Checkpoint — Pretty TTY Output

**Status**: Phase 3 Step 13 complete. Step 14 in flight (anti-AI + traceability).

## What was built

- TTY-aware output dispatcher (`output::emit` / `emit_to_stdout`) replacing the old `format_output(value, bool)` API.
- 4 new global CLI flags: `--json`, `--quiet` (`-q`), `--color={auto,always,never}`, `--no-color` (alias for `--color never`). Mutual-exclusion checks for `--json/--pretty` and `--color/--no-color` (D-7 / D-31).
- 19 curated per-command renderers in `pretty_renderer/curated/` (one file per renderer, D-30): session create/inspect/list/close/abort/replay/diff/validate/export, web navigate/generic_action/evaluate, vault add/list/grant_revoke, gc, doctor, import_playwright, benchmark.
- Recursive sensitive-field redaction (D-29) applied to curated + tail + fallback paths; the canonical-JSON path is intentionally NOT redacted (AC-TTY-02 byte-exactness).
- Per-stream color resolution (D-20) — stdout and stderr resolve their own `IsTerminal` + env-var ladder independently.
- "More details" tail block (D-21..D-24) — every receipt key not consumed by the curated renderer surfaces in a dim section, in JSON-Schema property order if available, else alphabetical (deterministic, no HashMap-iteration leak).
- Hand-rolled ANSI helper (D-4); zero new workspace dependencies.
- NO_COLOR spec correctness fix (D-16).
- 88 new tests covering AC-TTY-01..04 + every locked decision.

## Key files (path : intent)

### New
- `loom-cli/src/cli_config/output_mode.rs` — OutputMode enum + resolver
- `loom-cli/src/cli_config/color_choice.rs` — ColorChoice enum + per-stream resolver
- `loom-cli/src/command_router/validate_flags.rs` — mutex-flag rejector
- `loom-cli/src/pretty_renderer/ansi.rs` — ANSI const helpers
- `loom-cli/src/pretty_renderer/redact.rs` — recursive sensitive-field redaction
- `loom-cli/src/pretty_renderer/curated/mod.rs` — registry + dispatcher + tail block + rate-limited warning
- `loom-cli/src/pretty_renderer/curated/{session_*,web_*,vault_*,gc,doctor,import_playwright,benchmark,plural}.rs` — 19 files
- `loom-cli/tests/integration_tty_byte_exact.rs` — AC-TTY-02 (6 tests)
- `loom-cli/tests/integration_tty_pretty_golden.rs` — AC-TTY-01 (5 happy-path goldens)
- `loom-cli/tests/integration_tty_flags.rs` — AC-TTY-03/04 (17)
- `loom-cli/tests/integration_quiet_ids.rs` — AC-TTY-03 quiet (12)
- `loom-cli/tests/integration_tail_determinism.rs` (2)
- `loom-cli/tests/integration_malformed_receipts.rs` (8)
- `loom-cli/tests/integration_perf_regression.rs` (1)
- `loom-cli/tests/fixtures/pretty-golden/*.txt` (5 fixtures)

### Modified
- `loom-cli/src/cli_config/{cli_config.rs,mod.rs}` — added output_mode + per-stream color fields
- `loom-cli/src/command_router/{command_router.rs,mod.rs}` — added 4 globals + validate_flags
- `loom-cli/src/cli_main/cli_main.rs` — flag validation + per-stream resolve
- `loom-cli/src/error_mapper/error_mapper.rs` — color-aware print_error_with_color
- `loom-cli/src/output_formatter/output_formatter.rs` — emit + emit_to_stdout (legacy format_output kept)
- `loom-cli/src/pretty_renderer/{pretty_renderer.rs,mod.rs,interface_tests.rs}` — NO_COLOR spec fix + 4 new tests + module re-exports
- `loom-cli/src/{session,vault,admin,import,benchmark,action}_commands/*.rs` — 18 callsites migrated to emit_to_stdout
- `CHANGELOG.md` — Unreleased section with Added + Changed/breaking

## Open issues

None blocking. Pre-existing clippy lints in the workspace baseline (`module_inception`, `derivable_impls` in `error_mapper`, `doc_lazy_continuation` in `action_commands`, `manual_map` in `error_mapper`) are unrelated to this PR.

## Gate status

- ✅ `cargo check -p loom-cli`
- ✅ `cargo test -p loom-cli` — 402 passing, 0 failing, 6 ignored (pre-existing daemon-spawning E2E)
- ✅ `cargo clippy -p loom-cli` — no NEW lints (4 pre-existing baseline lints documented)

## Anti-AI review

`anti-ai-review.md` (z-ai/glm-4.7). 13/15 rules CLEAN. 2 findings:

1. **"Critical syntax error in `warn_once`"** — **HALLUCINATION**. The
   model claimed `warn_once` had an undefined `lock` variable and
   incorrect static initialisation; the suggested fix is byte-identical
   to the actually-shipping code. The 402 passing tests prove the code
   compiles and runs. Dismissed with this note.
2. **Minor: `let _ = emit_to_stdout(...)` in URL-rejection path** —
   **REAL, FIXED**. `loom-cli/src/action_commands/action_commands.rs`
   silently dropped emit_to_stdout's Result on the URL-allowlist
   rejection path. Now logs a stderr warning on emit failure while
   still propagating the original allowlist error (which is the
   actionable result for the user).

## Traceability

`traceability.md` — 16/16 plan items mapped to code + tests, 0 gaps,
0 orphaned tests. **100% coverage**.

## Test spec path

EXCEPTION: pure-backend. Tests defined in plan.md P3-8..P3-11c; implementations under `loom-cli/tests/integration_tty_*.rs`. No frontend agentic-tests applicable.

## Council plan review path

`specs/2026-05-04-cli-pretty-tty/council-plan-review.md/` (per-role files) + `council-plan-review-summary.md` (aggregated; final verdict APPROVE_WITH_CONDITIONS, all 6 conditions captured in D-29..D-34).
