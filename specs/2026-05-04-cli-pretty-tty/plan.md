# Plan — Pretty TTY Output for loom-cli

**Slug**: `cli-pretty-tty` | **Mode**: BUILD | **Size**: M | **Branch**: `cli-pretty-tty`
**PR title**: `feat(loom-cli): pretty TTY output with --json/--pretty/--quiet (AC-TTY-01..04)`

## WHAT

Replace today's "canonical JSON always" stdout policy with a TTY-aware policy:
- **At a TTY (stdout `IsTerminal`)**: emit human-readable colored multi-line projection of the receipt (per-command curated layout + tail block of unrendered fields).
- **Not at a TTY (pipe/file/CI)**: emit canonical JSON, byte-for-byte identical to today (AC-TTY-02 regression).
- Override flags: `--quiet > --json > --pretty > auto-detect` (`--json`/`--pretty` mutually exclusive without `--quiet`).
- `--no-color` flag and spec-correct `NO_COLOR` env disable color in pretty mode.

## WHY

ACs in original spec (verbatim):
- **AC-TTY-01** — At TTY default is human-readable; session create shows `session=01k... created`; action shows status + final_url + action_hash + console_count + network_summary in colored multi-line.
- **AC-TTY-02** — Non-TTY JSON output is bit-for-bit identical to today (regression test pinned at byte level).
- **AC-TTY-03** — `--json`/`--pretty` flags override autodetection. `--quiet` suppresses everything except errors and the final action_hash (one line).
- **AC-TTY-04** — `--no-color` respected; honor `NO_COLOR` env per no-color.org spec.

Out of scope per spec: TUI / interactive mode.

Phase 2 council intake additions (decisions.md D-7..D-17): flag precedence, universal `--quiet` rule, "more details" tail block, error-prose color at TTY, fix to `NO_COLOR` empty-string bug, `--no-color` flag.

## HOW (implementation steps, in order)

### Phase 3a — Foundation [AI_CODE]

**Step P3-1: Add `OutputMode` enum + flag plumbing** [AI_CODE]
- New file: `loom-cli/src/cli_config/output_mode.rs` — `pub enum OutputMode { Quiet, Json, PrettyCurated, PrettyFallback }`. (Fallback only used internally when curated renderer is missing or returns Err per D-23.)
- New file: `loom-cli/src/cli_config/color_choice.rs` — `pub enum ColorChoice { Auto, Always, Never }` (D-22).
- Modify `loom-cli/src/cli_config/cli_config.rs`:
  - Replace `pub pretty: bool` with `pub output_mode: OutputMode` (default = `Json` per IC-CLI-01 — the resolver overwrites from auto-detect at startup).
  - Add `pub stdout_color_enabled: bool` AND `pub stderr_color_enabled: bool` (D-20). Each resolved independently from per-stream `IsTerminal` + env vars.
  - Update `compiled_defaults`, file/env override mapping (`pretty` env-var still recognised but maps to `output_mode = PrettyCurated` for back-compat with `LOOM_PRETTY=true`).
- Modify `loom-cli/src/command_router/command_router.rs`:
  - Add globals: `pub json: bool`, `pub quiet: bool`, `pub color: Option<ColorChoice>` (clap value-enum: `auto|always|never`), and keep `pub pretty: bool`. Add `--no-color` as clap alias (`#[arg(long = "no-color", visible_alias)]` mapped to `--color never`) per D-22.
  - Add a `validate_flags` helper that errors on `--json --pretty` conflict (return `CliError::Usage`) per D-7.
- Modify `loom-cli/src/cli_main/cli_main.rs`:
  - In `async_run`, after `try_parse_from`, call `validate_flags`, then resolve `OutputMode` per precedence: `quiet > json > pretty > auto-detect`. Auto-detect = `std::io::stdout().is_terminal()` ? `PrettyCurated` : `Json`.
  - Resolve color enablement per stream (D-20, D-22). For each of stdout / stderr:
    1. `--color=always` → enabled.
    2. else `--color=never` (or `--no-color`) → disabled.
    3. else `CLICOLOR_FORCE` non-empty → enabled.
    4. else `NO_COLOR` non-empty (D-16 spec-correct) → disabled.
    5. else `CLICOLOR=0` → disabled.
    6. else `TERM=dumb` → disabled.
    7. else `IsTerminal(this stream)`.

**Step P3-1b: `validate_flags` reject conflicts** [AI_CODE] (D-7, D-31)
- New helper `loom-cli/src/command_router/validate_flags.rs`:
  - Reject `--json --pretty` together (D-7) → `CliError::Usage("--json and --pretty are mutually exclusive")`.
  - Reject `--color X --no-color` together (D-31) → `CliError::Usage("--color and --no-color are mutually exclusive")`.
  - Both checks examine raw clap matches (not resolved values) so the Usage error fires even when both flags would reduce to the same effective output.

**Step P3-2: New stdout entry point `output::emit`** [AI_CODE]
- Modify `loom-cli/src/output_formatter/output_formatter.rs`:
  - Add `pub fn emit(method: &str, value: &serde_json::Value, cfg: &CliConfig) -> Result<String, CliError>` that dispatches on `cfg.output_mode`:
    - `Quiet` → call `quiet_id_for(method, value)` — single-line id or empty string (suppress newline if empty).
    - `Json` → `serde_jcs::to_string(value)` (byte-exact AC-TTY-02 path).
    - `PrettyCurated` → call into new `curated_renderers::render(method, value, cfg)`, falling through to `PrettyFallback` if no curated renderer matches.
    - `PrettyFallback` → existing `PrettyRenderer::render` + new tail block.
  - Drop the `format_output(value, pretty: bool)` function in favour of `emit`. Update all 18 callsites to `let bytes = emit(method, &resp, cfg)?;` propagating any error via `?` per D-25 — no `unwrap`/`expect`. The `Err` case is a `CliError::Internal` (canonical-JSON failure or malformed-receipt --quiet id-extraction failure).
  - The two field-projecting handlers (`session inspect`, `session diff`) keep extracting their sub-field and pass the sub-field + the parent method id (`session.inspect`, `session.diff`) to `emit`. Curated renderers for those methods are designed against the projected shape.
- Migrate `command_router.rs:216` (doctor) to call `emit("doctor", &report, cfg)` and route through the new pipeline. (Carve-out resolved.)
- Carve-outs that DO NOT route through `emit`: `version_command.rs:46` (SR-CLI-01 latency budget), `serve_runner.rs:62` (HELLO_TOKEN), `session_commands.rs:505-508` (binary export bytes). Documented inline.

**Step P3-3: ANSI helper module** [AI_CODE]
- New file: `loom-cli/src/pretty_renderer/ansi.rs`:
  - `pub const RESET: &str`, `BOLD`, `DIM`, `RED`, `GREEN`, `YELLOW`, `CYAN`.
  - `pub fn paint(s: &str, code: &str, color_enabled: bool) -> String` — passthrough when `!color_enabled`.
- Fix the `NO_COLOR` spec bug:
  - In `pretty_renderer/pretty_renderer.rs:detect_color_enabled`, change to `std::env::var("NO_COLOR").map(|s| !s.is_empty()).unwrap_or(false)` semantics (var present and non-empty → disable). Add a `#[test]` in `pretty_renderer/interface_tests.rs` covering the three spec cases (unset / set-empty / set-non-empty).

**Step P3-4: `quiet_id` per-renderer trait method** [AI_CODE] (was D-8 centralized; revised to D-19)
- The `CuratedRenderer` trait (defined in P3-5) gets:
  ```rust
  fn quiet_id(&self, value: &serde_json::Value) -> Option<String> { None }
  ```
  Default `None` = silent under `--quiet`. Each renderer overrides per D-8 universal rule:
  - `SessionCreate::quiet_id` → `value["session_id"]`
  - `SessionClose / SessionAbort / SessionReplay / SessionValidate::quiet_id` → `value["session_id"]`
  - `SessionInspect / SessionDiff::quiet_id` → `None` (no top id in projected sub-field)
  - `SessionList::quiet_id` → newline-joined `session_id`s from the array
  - `SessionExport::quiet_id` → `value["artifact_ref"]`
  - `WebGenericAction::quiet_id` (covers all `web.*`) → `value["action_hash"]`
  - `VaultAdd / VaultGrantRevoke::quiet_id` → `value["grant_id"]`
  - `VaultList::quiet_id` → newline-joined `grant_id`s
  - `Gc / Doctor / ImportPlaywright / Benchmark::quiet_id` → `None` (silent at --quiet)
- The `output::emit` dispatcher, when `cfg.output_mode == Quiet`:
  1. Look up `registry().get(method)` — if found, call `quiet_id(value)`.
  2. If `Some(id)` → return `format!("{id}\n")`.
  3. If `None` (no renderer or renderer returned None) → return empty `String` (caller writes nothing — no trailing newline).
- For methods with NO curated renderer, `--quiet` is silent. Document this in CHANGELOG so users don't expect every command to support `--quiet` output.

### Phase 3b — Curated renderers [AI_CODE]

**Step P3-5: Curated renderer registry + tail block** [AI_CODE] (revised per D-23, D-24, D-18, D-29, D-30, D-32)
- New module DIRECTORY (D-30): `loom-cli/src/pretty_renderer/curated/` with `mod.rs` + one file per renderer (~18 files, see file-touch summary below).
- `mod.rs` content:
  - ```rust
    pub struct RenderedReceipt {
        pub text: String,
        pub consumed_keys: HashSet<String>,
    }
    pub trait CuratedRenderer: Send + Sync {
        fn render(&self, value: &serde_json::Value, cfg: &CliConfig) -> Result<RenderedReceipt, CliError>;
        fn quiet_id(&self, value: &serde_json::Value) -> Option<String> { None }
    }
    ```
    `consumed_keys` is dynamic (D-23) — a renderer can decide at runtime to skip a null field, and the dispatcher will move it to the tail block.
  - `pub fn registry() -> &'static HashMap<&'static str, Box<dyn CuratedRenderer>>` — built lazily via `OnceLock`.
  - `pub fn render(method: &str, value: &serde_json::Value, cfg: &CliConfig, schemas: Option<&SchemaCache>) -> Result<String, CliError>`:
    1. If no renderer for `method` → return `Err(CliError::Internal("no curated renderer".into()))` — `emit` catches and falls through to `PrettyFallback`.
    2. Else call renderer's `render(value, cfg)`.
    3. **If renderer returns Err** (D-23 fallback path): emit a single stderr warning `curated render failed for <method>: <err>; degraded to fallback` (rate-limited per D-32: at most once per method per process via `OnceLock<Mutex<HashSet<&'static str>>>`) and return `PrettyFallback`'s output instead.
    4. **Pre-redact the input value** (D-29 recursive redaction): every renderer receives a `redact_recursive(value)` view where any field name (at any nesting level) matching `(?i)(token|secret|password|api_?key|bearer|credential|cookie|jwt|session_?key|access_?token|refresh_?token|private_?key|signing_?key|client_?secret|oauth)` has its value replaced with the JSON string `"<redacted>"`. The expanded regex per test_engineer's request. Same redaction applied to `PrettyFallback`. **The `--json` path is NOT redacted** (preserves AC-TTY-02 byte-exactness).
    5. On Ok(rendered): compute tail keys = `value_object_keys ∖ rendered.consumed_keys`. Order them per **D-24**: if `schemas.response_schema(method)` is Some, use the JSON-Schema `properties` order; else **sort alphabetically** (NOT HashMap iteration order, which is randomised per process — would break golden tests).
    6. Append tail (when non-empty): blank line + `── more ──` (DIM) + `key: value` lines (DIM-keyed). When `value` has zero unrendered keys, suppress the entire tail block (no separator, no blank line).
- New file: `loom-cli/src/pretty_renderer/redact.rs` exposing `pub fn redact_recursive(value: &serde_json::Value) -> serde_json::Value` and `pub fn is_sensitive_key(key: &str) -> bool` (with the regex). Unit tests cover all expanded patterns and at least 3 levels of nesting.

**Step P3-6: Curated layouts per command** [AI_CODE] (~15 commands)
- For each method, implement a struct in `curated.rs` (e.g. `SessionCreate`, `WebNavigate`, `WebGenericAction`, `SessionList`, `SessionInspect`, `SessionDiff`, `SessionValidate`, `SessionExport`, `SessionClose`, `SessionAbort`, `SessionReplay`, `VaultAdd`, `VaultList`, `VaultGrantRevoke`, `Gc`, `Doctor`, `ImportPlaywright`, `Benchmark`). 
- Layouts (sketch):
  - **`session.create`**: `<GREEN>session=<session_id></GREEN> <DIM>created</DIM>` (one line) + tail block of metadata fields.
  - **`web.navigate`**: 5 lines — `status: <colored ok/error>`, `final_url: <CYAN><url></CYAN>`, `action_hash: <hash>`, `console_count: <plural(n, "line")>`, `network_summary: <plural(total, "request")>, <bytes_human> bytes, <plural(error_count, "error")>` (D-28 pluralization helper). Tail of remaining fields (action_id, session_id, timing_ticks, outcome_hash, emitted_at_ms, etc.).
  - **`web.click/type/select/...`** (hash-only tier): single line `<GREEN>action_hash=<hash></GREEN>` + tail (outcome_hash, emitted_at_ms).
  - **`web.evaluate`**: `action_hash` + `return_value` (truncate JSON > 200 chars with "(use --json for full value)") + blob_ref if present + tail.
  - **`session.list`**: table-style (D-26: id=26, status=10, created_at=20, total 58 cols, 80-col safe). Header row DIM. Blank line separator. Long values truncate with `…` at column boundary. **Empty list (D-21): render `<DIM>No sessions found.</DIM>` and skip the table entirely.**
  - **`session.inspect`** (renders the projected manifest_summary value, since the handler already extracts `.manifest_summary`): summary fields + tail.
  - **`session.diff`** (renders the projected `.diff` value): summary line `<plural(n, "field diff")>, action_count_delta=<n>` (D-28). Then per-diff lines (D-27): `  <RED>- key: old_value</RED>` followed by `  <GREEN>+ key: new_value</GREEN>` (2-space indent). Values JSON-stringified inline; truncate >80 chars with `…`. Then tail.
  - **`session.close/abort/replay`**: `<GREEN>session=<id></GREEN> <verb-past-tense>` + tail.
  - **`session.validate`**: keep existing PASS/FAIL output (already TTY-friendly), color PASS green / FAIL red, color reasons dim.
  - **`session.export`**: skip — binary path stays bytes-through.
  - **`vault.add/grant/revoke`**: id + status one-liner + tail.
  - **`vault.list`**: table by grant_id (same column convention as session.list). **Empty list (D-21): `<DIM>No vault entries.</DIM>`.**
  - **`gc.run`**: `<GREEN>gc</GREEN> deleted=<n> bytes=<n>` + tail.
  - **`doctor`**: header line per check; `<GREEN>OK</GREEN>` / `<YELLOW>WARN</YELLOW>` / `<RED>FAIL</RED>` glyphs.
  - **`import.playwright`**: `<GREEN>imported session=<id></GREEN>` + tail.
  - **`benchmark`**: status + summary table.

**Step P3-7: Error path color** [AI_CODE] (revised per D-20)
- Modify `loom-cli/src/error_mapper/error_mapper.rs:print_error` (line 305): when `cfg.stderr_color_enabled` (D-20: independent of stdout color), wrap the error message with `RED + BOLD ... RESET`. (Today: plain `eprintln!`.) Plumb `cfg` into `print_error` (currently it takes `&Result`).
- Action error path (`action_commands.rs:90-92`) JSON receipt on stdout stays AS IS — bytes preserved (AC-TTY-02 covers it). The receipt's `format_output(v, cfg.pretty)` callsite migrates to `emit("<method>", v, cfg)` like every other receipt site.

### Phase 3c — Tests [AI_CODE + AI_RESEARCH]

**Step P3-8: Byte-exact AC-TTY-02 regression** [AI_CODE]
- New test file: `loom-cli/tests/integration_tty_byte_exact.rs`
  - Spawn `loom session create --socket=mock` as a subprocess (stdout NOT a TTY). Capture stdout bytes. Assert `bytes == serde_jcs::to_string(receipt) + "\n"`. Repeat for: `loom session list`, `loom action web.navigate`, etc. (one fixture per major method).
  - Fixtures live at `loom-cli/tests/fixtures/canonical-receipts/<method>.json`.
- New unit test: `loom-cli/src/output_formatter/interface_tests.rs::ac_tty_02_emit_json_path_byte_exact` — calls `emit(method, &value, cfg{output_mode:Json})` and asserts equality with `serde_jcs::to_string(value) + "\n"`.

**Step P3-9: TTY-mode golden tests** [AI_CODE] (scoped per D-34)
- New test file: `loom-cli/tests/integration_tty_pretty_golden.rs` — uses `loom_cli::run` in-process, force `output_mode=PrettyCurated`, `stdout_color_enabled=true`. Compare bytes against fixtures.
- **Scoped to happy-path receipts (D-34): 5 golden fixtures** — `session_create_at_tty.txt`, `web_navigate_at_tty.txt`, `gc_at_tty.txt`, `doctor_at_tty.txt`, `error_red_at_tty.txt`. Edge cases (empty list, redaction match, tail block ordering, --quiet IDs, flag conflicts) covered by byte-shape unit tests in P3-11/P3-11b/P3-11c, NOT golden text fixtures.

**Step P3-10: Flag precedence + --quiet tests** [AI_CODE]
- New test file: `loom-cli/tests/integration_tty_flags.rs`:
  - `--json --pretty` → exit 2 + Usage error (AC-TTY-03 conflict).
  - `--quiet --json` together → emits id only on stdout, JSON nowhere.
  - `--color=never` / `--no-color` → no ANSI bytes even with `--pretty`.
  - `--color=always` with stdout piped → ANSI bytes ARE present (force color into pipe per D-22).
  - `CLICOLOR_FORCE=1` env with stdout piped → ANSI bytes present.
  - `NO_COLOR=1` env → no ANSI bytes.
  - `NO_COLOR=""` env (empty) → ANSI bytes still present (D-16 spec correctness).
  - `CLICOLOR=0` env → no ANSI bytes.
  - `TERM=dumb` env → no ANSI bytes.
  - Stdout pipe + stderr TTY: stdout has no ANSI; stderr error message DOES have color (D-20 per-stream resolution).
  - Auto-detect with stdout=pipe → JSON.

**Step P3-11: --quiet per-command identity tests** [AI_CODE]
- New test file: `loom-cli/tests/integration_quiet_ids.rs`:
  - `loom session create --quiet` → stdout is `<session_id>\n`.
  - `loom action web.click ... --quiet` → stdout is `<action_hash>\n`.
  - `loom session list --quiet` → stdout is `<id>\n<id>\n...`.
  - `loom session list --quiet` with empty list → stdout is empty (no header, no message).
  - `loom gc --quiet` → empty stdout.
  - `loom session inspect --quiet` → empty stdout (`SessionInspect::quiet_id` returns None per D-19).
  - On error: stdout still gets the JSON error receipt (action error path); stderr gets the prose. `--quiet` does NOT silence error stderr (D-10).

**Step P3-11b: Tail block + redaction tests** [AI_CODE] (D-18 / D-23 / D-24 / D-29 / D-32)
- New unit tests in `loom-cli/src/pretty_renderer/curated/mod.rs#[cfg(test)]` and `redact.rs`:
  - Tail block deterministic: with no schema, repeated runs produce alphabetically-sorted tail (D-24).
  - Tail block uses schema property order when SchemaCache has the schema.
  - Recursive sensitive-field redaction (D-29): receipt with `{"top": {"nested": {"api_key": "abc"}}}` → all paths render `"<redacted>"`.
  - Expanded regex coverage (D-29): test cases for `auth_token`, `private_key`, `oauth_token`, `access_token`, `refresh_token`, `signing_key`, `client_secret`, `bearer`, `cookie`, `jwt`.
  - `--json` path bypasses redaction (AC-TTY-02 byte-exactness preserved).
  - Renderer Err → fallback to PrettyRenderer + single stderr warning (D-23).
  - Warning rate-limit (D-32): repeated failures for same method emit only ONE stderr warning per process.
  - Empty list (D-21): SessionList with empty array → "No sessions found." line, no header table.

**Step P3-11c: Determinism + malformed receipt + perf** [AI_CODE] (D-33)
- `loom-cli/tests/integration_tail_determinism.rs::tail_order_byte_exact_under_no_schema` — render same receipt 100 times, assert identical bytes (D-24 + D-33).
- `loom-cli/tests/integration_malformed_receipts.rs` — fuzz-style: missing required fields, wrong types, deeply-nested null, truncated arrays. Each asserts: no panic, fallback to PrettyFallback path, stderr warning emitted (rate-limited).
- `loom-cli/tests/integration_perf_regression.rs::session_list_1000_items_under_100ms` — generate 1000-session mock receipt; time `emit("session.list", &value, cfg{Pretty})`; assert `<100ms` locally (relaxed to `<500ms` under `CI=true` env). Doc comment notes this is best-effort.
- `loom-cli/src/output_formatter/interface_tests.rs::all_method_emitters_have_renderer_or_silent_quiet_id` — runtime test enumerating every method string used in handler `emit(...)` calls (compile-time `const SUBCOMMAND_RPC_MAP` + `web.*` aliases registry), asserting each is in `curated::registry()` OR documented as silent under --quiet. (D-33 method-mismatch guard.)

### Phase 3d — Documentation

**Step P3-12: CHANGELOG entry + per-command --help text** [AI_CODE]
- `CHANGELOG.md`: Add **Breaking** section noting `--pretty` repurposed (was indented JSON; now human prose). Add `--json`, `--quiet`, `--no-color`, `--color=auto|always|never` flag entries.
- For each global flag, ensure clap `#[arg(long, global = true, help = "...")]` text describes auto-detect + override behavior + per-command `--quiet` output. (Touch ~5 doc strings.)
- Document the `--quiet` rule per-command in the help text:
  - global flag help: `Suppress non-error output. For commands that produce a single resource (session create, action), prints only the canonical id. For list commands (session list, vault list), prints one id per line — may be a large amount of data on big result sets.`

### Phase 3e — Untangle `cfg.pretty` [AI_CODE]

**Step P3-13: Migrate cfg.pretty readers** [AI_CODE]
- All 16 readers of `cfg.pretty` (per scan B3) now read `cfg.output_mode`. Most just pass it to `emit`. The handful that branch on it (`benchmark_commands.rs:45`) update to switch on the enum.

## File-touch summary (estimate)

**Modified** (~10 files):
- `loom-cli/src/output_formatter/output_formatter.rs`, `interface_tests.rs`
- `loom-cli/src/pretty_renderer/pretty_renderer.rs`, `interface_tests.rs`
- `loom-cli/src/cli_config/cli_config.rs`, `interface_tests.rs`
- `loom-cli/src/command_router/command_router.rs`, `interface_tests.rs`
- `loom-cli/src/cli_main/cli_main.rs`
- `loom-cli/src/error_mapper/error_mapper.rs`
- `loom-cli/src/session_commands/session_commands.rs`
- `loom-cli/src/action_commands/action_commands.rs`
- `loom-cli/src/vault_commands/...rs`
- `loom-cli/src/admin_commands/...rs`
- `loom-cli/src/import_commands/...rs`
- `loom-cli/src/benchmark_commands/impl_benchmark.rs`
- `loom-cli/src/version_command.rs` (no change — carve-out)
- `loom-cli/src/serve_runner.rs` (no change — carve-out)
- `CHANGELOG.md`

**New** (~6 files):
- `loom-cli/src/cli_config/output_mode.rs`
- `loom-cli/src/output_formatter/quiet_id.rs`
- `loom-cli/src/pretty_renderer/ansi.rs`
- `loom-cli/src/pretty_renderer/curated.rs` (or split into `curated/` dir if it grows)
- `loom-cli/tests/integration_tty_byte_exact.rs`
- `loom-cli/tests/integration_tty_pretty_golden.rs`
- `loom-cli/tests/integration_tty_flags.rs`
- `loom-cli/tests/integration_quiet_ids.rs`
- `loom-cli/tests/fixtures/canonical-receipts/*.json`
- `loom-cli/tests/fixtures/pretty-golden/*.txt`

**Net LOC estimate** (revised post council): +1500/-180 (curated renderers split across 18 files ~700, tests ~500, ANSI/quiet/output-mode/redaction ~200, flag/config plumbing + validate_flags ~100). One file per renderer is roughly the same total LOC as a monolith, distributed.

## Risks

1. **`cfg.pretty: bool` → `cfg.output_mode: OutputMode` is a breaking config change.** Anyone with `pretty = true` in `~/.config/loom/config.toml` keeps working via back-compat in `apply_overrides`. CHANGELOG calls this out.
2. **`format_output` removal.** 18 call sites change. Risk of accidental byte-shift if a callsite forgets to pass the method name. Mitigated by the new `emit` signature requiring `method: &str` (compile-time enforcement).
3. **Curated renderers vs. evolving daemon receipts.** The "more details" tail block (D-9) ensures no info is hidden. Golden tests will fail when daemon adds fields — surfacing the need to update the curated layout or accept it being in tail.
4. **Subprocess test cost.** `integration_tty_byte_exact.rs` spawns the binary per assertion. Use a `tempdir`-pooled mock daemon (existing `mocks::` patterns) to amortize.
5. **NotTTY in golden tests.** `integration_tty_pretty_golden.rs` runs in-process so we can force `output_mode=PrettyCurated`. Tests don't depend on the test runner's actual TTY status.

## Acceptance evidence map

| AC | Test |
|---|---|
| AC-TTY-01 (TTY default human) | `integration_tty_pretty_golden::session_create_at_tty_shows_session_id_created`, `..._action_navigate_shows_status_and_url` |
| AC-TTY-02 (non-TTY byte-exact) | `integration_tty_byte_exact::*` (subprocess) + `output_formatter::interface_tests::ac_tty_02_emit_json_path_byte_exact` (unit) |
| AC-TTY-03 (--json/--pretty/--quiet) | `integration_tty_flags::*`, `integration_quiet_ids::*` |
| AC-TTY-04 (--no-color, NO_COLOR) | `integration_tty_flags::color_never_flag_strips_ansi`, `..::no_color_env_strips_ansi`, `..::empty_no_color_does_not_strip` (NEW D-16 spec correctness), `..::color_always_forces_color_in_pipe`, `..::clicolor_force_forces_color_in_pipe` (D-22) |

## Item tags
[AI_CODE] all implementation steps. No [HUMAN_ACTION] required (no infra/credentials/migrations). [AI_RESEARCH] resolved in Phase 1 Step 4.

## Plan revisions (post-review)

Plan v1 was reviewed by:
- **Gemini 2.5 Pro** (cross-model review) — `gemini-review.md`. Found 6 issues.
- **GLM-4.7** (ambiguity detection) — `ambiguity-report.md`. Found 6 findings (3 critical, 3 advisory).

All 12 issues incorporated (decisions D-18 through D-28 in `decisions.md`):
- D-18 Sensitive-field redaction in tail block (Gemini G1)
- D-19 Per-renderer `quiet_id` (Gemini G2)
- D-20 Independent stdout/stderr color flags (Gemini G3)
- D-21 Empty-state copy for list commands (Gemini G4)
- D-22 `--color=auto|always|never` + `CLICOLOR_FORCE`/`CLICOLOR` (Gemini G5)
- D-23 Dynamic `consumed_keys` via `RenderedReceipt` + Err-fallback (Gemini G6 + Ambiguity A4)
- D-24 Deterministic tail order (alphabetical when no schema) (Ambiguity A1)
- D-25 `emit()` callsites use `?` (Ambiguity A5)
- D-26 `session.list` table column widths (Ambiguity A2)
- D-27 `session.diff` per-diff line format (Ambiguity A3)
- D-28 Pluralization helper (Ambiguity A6)

No plan v2 cross-model review needed: every G/A finding is non-conflicting (each is a tightening, not a redirection); plan converged in one iteration.

### Step 11 council review (5 reviewers)

Plan v2 reviewed by `council/review.py` with roles `security,code_quality,test_engineer,devil,process_critic`. Per-role transcripts in `council-plan-review.md/`. Aggregated summary in `council-plan-review-summary.md`. 4× APPROVE_WITH_CONDITIONS, 1× BLOCK (security — rubric mismatch, see summary). Net 6 new decisions:
- D-29 Recursive sensitive-field redaction (curated + tail + fallback)
- D-30 Curated renderers in `curated/` directory (one file per renderer)
- D-31 `--color X --no-color` conflict → Usage error
- D-32 Renderer-fallback warning rate-limited (once per method per process)
- D-33 Determinism / malformed-receipt / perf regression / method-registry tests
- D-34 Golden-file scope trimmed to 5 happy-path fixtures (process_critic)

Plan v3 incorporates all six. Final.
