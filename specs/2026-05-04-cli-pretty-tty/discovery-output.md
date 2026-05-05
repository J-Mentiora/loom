# Discovery Output — Pretty TTY Output for loom-cli

## Mode & Size

- **Mode**: BUILD
- **Size**: **M** (bumped from initial S after Step 3 scope decision — curated layouts for all subcommands rather than only the two AC-named ones)
- **PR title** (locked): `feat(loom-cli): pretty TTY output with --json/--pretty/--quiet (AC-TTY-01..04)`

## Feature (1-line)

`loom session create` and `loom action ...` (and other receipt-emitting subcommands) auto-detect whether stdout is a TTY: pretty colored multi-line at TTY, byte-exact canonical JSON when piped. `--json` / `--pretty` / `--quiet` / `--no-color` flags + `NO_COLOR` env override auto-detection.

## Acceptance criteria (verbatim)

- **AC-TTY-01** — When stdout `isatty(1)`, default output is human-readable: session create shows `session=01k... created`; action shows status + final_url + key receipt fields (action_hash, console_count, network_summary) in colored multi-line format.
- **AC-TTY-02** — When stdout is NOT a tty, JSON output is bit-for-bit identical to today (regression test pinned at the byte level).
- **AC-TTY-03** — `--json` and `--pretty` flags override auto-detection. `--quiet` suppresses everything except errors and the final action_hash (one line).
- **AC-TTY-04** — `--no-color` respected; honor `NO_COLOR` env per no-color.org.

## Context category & domain preset

- Category: **tooling**
- Domain preset: **tooling** (`process_critic`, `code_quality` weight boosts for Phase 2 council)

## Invariant docs loaded

In-tree contracts (no `invariants/` or `code_specs/` folders exist):
- `loom-cli/src/output_formatter/output_formatter.rs` — IC-CLI-01 (canonical JSON default), IC-CLI-02 (--pretty → PrettyRenderer), SR-CLI-03 (receipt pass-through).
- `loom-cli/src/pretty_renderer/pretty_renderer.rs` — schema-driven renderer; NO_COLOR / TERM=dumb compliance.
- `loom-cli/src/cli_config/cli_config.rs` — BC-CLI-02 precedence (CLI > env > file > defaults), AC-CFG-03.1 schema validation.
- `loom-cli/src/cli_main/cli_main.rs` — AC-CLIOUT2-03 (`Cli.pretty` mirrored to `cfg.pretty`); IC-CLI-04 (sole `process::exit` caller).
- `loom-cli/src/command_router/command_router.rs` — `Cli` parser; global flags.
- `loom-cli/src/{session,action,vault,admin,import,benchmark,version}_commands/` — handler shapes & receipt projections.
- `loom-cli/tests/cli_integration.rs` — existing AC-CLI-04.1 / 04.2 pins (canonical JSON parseable; no ANSI; NO_COLOR / TERM=dumb honored).

## Light scan summary (top 3)

1. **One choke-point exists**: `output_formatter::format_output(value, pretty)` is called from 18 of 21 stdout sites. Refactoring it is the right place to install TTY auto-detection. 3 outliers are explicit carve-outs:
   - `version_command.rs:46` — direct serde_json::to_string for SR-CLI-01 latency budget — leave as JSON.
   - `serve_runner.rs:62` — `HELLO_TOKEN=<hex>` daemon handshake — leave verbatim.
   - `session_commands.rs:505-508` — binary artifact write (`session export`) — leave bytes-through.
   Plus `command_router.rs:216` (doctor direct JSON) which we WILL re-route through the new formatter.
2. **Existing `--pretty` is indented JSON only** — the schema-driven `PrettyRenderer` exists but no handler reaches it because every handler calls `format_output(value, cfg.pretty)`, which falls into `serde_json::to_string_pretty` when `pretty=true`. This means repurposing `--pretty` to "human-readable colored" (Step 3 Q1) is essentially free of regression risk for `--pretty` users — they're already getting basic indented JSON.
3. **TTY detection is greenfield** — zero use of `is_terminal`/`atty`/`std::io::IsTerminal`. MSRV is 1.92 → stdlib `IsTerminal` is fine; no new workspace dep.

Full inventory: see `codebase-scan-light.md`.

## Open questions (none blocking)

None. All four Step 3 questions answered. Phase 2 will turn the answers into a plan.

## Round 1 Q&A (locked)

| # | Question | Answer | Implication |
|---|---|---|---|
| 1 | `--pretty` semantics? | **Repurpose: --pretty = human** | Existing `--pretty` users (who got indented JSON) now get the colored multi-line output. Document in CHANGELOG. Drop the `serde_json::to_string_pretty` branch from `format_output`. |
| 2 | Scope — which commands get pretty rendering? | **All commands; tailored projections everywhere** | Curated layouts for ~15 subcommands. Schema-driven generic fallback NOT used (would still be wired as last-resort when a tailored renderer is missing). Triggered the size bump S → M. |
| 3 | `--quiet` behavior for non-action commands? | **session create: print session_id; mirror for similar commands** | `loom session create --quiet` → just session_id. `loom session close --quiet` → session_id. `loom action ... --quiet` → action_hash. Other commands' --quiet output: print primary identifier from receipt if there is one; else silent. Errors always to stderr. |
| 4 | Color implementation? | **Hand-rolled ANSI helper, no new dep** | Build a tiny `ansi.rs` module with const `RESET`, `BOLD`, `DIM`, `RED`, `GREEN`, `YELLOW`, `CYAN`. Zero new workspace deps; no deny.toml updates. |

## Decisions locked from Phase 1

- Use `std::io::IsTerminal` (no new dep).
- Hand-rolled ANSI (no new dep).
- `--pretty` repurposed; old indented-JSON behavior dropped (CHANGELOG entry required).
- `--quiet` prints the primary receipt identifier where one exists.
- Tailored multi-line layouts for every receipt-emitting subcommand; schema-driven `PrettyRenderer` becomes the safety-net fallback for any method without a tailored renderer (and for receipts that fail to deserialize into the tailored shape).
- Three carve-outs (version, serve HELLO, session export) keep their existing output verbatim regardless of TTY/--pretty/--json.

## Phase 1 retrospective

**Skipped** per template SIZE GATE — "M features defer the retrospective to Phase 4 (Step 17)."
