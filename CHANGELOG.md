# Changelog

All notable changes to loom are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Added — Pretty TTY output (AC-TTY-01..04)

- **TTY auto-detection.** `loom session create` and `loom action ...`
  (and every other receipt-emitting subcommand) now auto-detect whether
  stdout is a terminal. At a TTY: human-readable colored multi-line
  output. Piped or redirected: canonical JSON, byte-for-byte identical
  to the previous default (regression-pinned via
  `tests/integration_tty_byte_exact.rs`).
- **`--json`** global flag — force canonical JSON, even when stdout is
  a TTY. Mutually exclusive with `--pretty`.
- **`--quiet` / `-q`** global flag — suppress everything except errors
  and the canonical resource id. `loom session create --quiet` prints
  just the session_id; `loom action ... --quiet` prints just the
  action_hash; `loom session list --quiet` prints one id per line.
  Errors always go to stderr.
- **`--color={auto,always,never}`** global flag (with `--no-color` as
  a convenience alias for `--color never`). Honours `NO_COLOR`,
  `CLICOLOR_FORCE`, `CLICOLOR=0`, and `TERM=dumb` per their respective
  conventions in `auto` mode.
- **Per-stream color resolution.** Stdout and stderr color are decided
  independently (`std::io::IsTerminal` per stream) so a piped stdout
  can still ship colored stderr error prose.
- **Recursive sensitive-field redaction in pretty output.** Receipt
  fields whose key (at any nesting level) matches a conservative
  denylist (`token`, `secret`, `password`, `api_key`, `oauth`,
  `credential`, `cookie`, `jwt`, `session_key`, `access_token`,
  `refresh_token`, `private_key`, `signing_key`, `client_secret`, etc.)
  render as `<redacted>` in the human path. The `--json` path is
  unchanged — machines still see the raw value, preserving AC-TTY-02
  byte-exactness.
- **Curated multi-line layouts** for every receipt-emitting subcommand:
  `session create / inspect / list / close / abort / replay / diff /
  validate / export`, `action <surface>.<verb>` (all `web.*` verbs),
  `vault add / list / grant / revoke`, `gc`, `doctor`,
  `import playwright`, `benchmark`. Each renderer declares the keys it
  consumed; remaining receipt fields surface in a dim "more details"
  tail block (in JSON-Schema property order if available, else
  alphabetically — never randomised).
- **Empty-state copy.** `session list` with no sessions prints
  `No sessions found.` instead of an empty header table; `vault list`
  empty prints `No vault entries.` (D-21).

### Changed — breaking

- **`--pretty` semantics changed.** Previously, `--pretty` produced
  indented JSON via `serde_json::to_string_pretty`. It now produces
  the human-readable colored multi-line layout described above. If
  your scripts depend on the old indented-JSON shape, switch to
  `--json` (canonical single-line, machine-parseable) — the canonical
  form is what you want for `jq` and similar pipelines anyway.
- **`NO_COLOR` empty-string behaviour fixed.** Previously, setting
  `NO_COLOR=""` (empty) was treated as a disable signal, contrary to
  the [no-color.org](https://no-color.org/) spec. The variable now
  disables color only when set to a non-empty value, as the spec
  requires. CI environments that set `NO_COLOR=""` for parent
  processes will no longer accidentally disable color.

## [1.0.0] — 2026-05-04

First stable release. Extracted from Mentiora's code-pipeline project
after 23 rounds of GA-driven hardening. API is stable; breaking changes
will bump the major version.

### Added

- **Determinism harness.** `Math.random()` (sfc32-seeded) and
  `Date.now()` / `performance.now()` (session-fixed epoch_ms) are
  pinned at session-create time and reproduced bit-for-bit on replay
  via the manifest's `started_at_ms`.
- **Implicit session management** in `loom-mcp serve`. MCP clients
  call `loom.web.navigate` with just `url` — the server lazily creates
  a session on first tool call and closes it on shutdown.
- **Path-traversal-safe session IDs.** All five `core_api_facade`
  paths reject anything that isn't a 26-char lowercase-alphanumeric
  ULID. Prevents file-disclosure via `session.inspect '../evil'`.
- **Typed error mapping.** `kind: "http_status"` (with status code) for
  4xx/5xx responses, `kind: "wait_predicate_false"` for `web.wait`
  selector misses, `kind: "schema_violation"` for unsupported export
  formats with the actual supported list in the message.
- **Chromium subprocess crash detection.** `ChromiumSupervisor` now
  spawns a tokio task that `await`s `child.wait()`; the moment chromium
  dies, the shim self-exits → daemon's crash watcher fires → in-flight
  CDP commands fail in <1s instead of waiting the 30s recv timeout.
- **GC reference protection.** `collect_referenced_blobs` decodes
  `receipt_canonical_bytes` byte arrays and recursively scans for hex
  hashes, so `loom gc --ttl 0` no longer deletes blobs that active
  sessions still reference.
- **Stable session listing.** `loom session list` is now sorted by
  `created_at_ms` descending with session_id as tiebreak — independent
  of filesystem read_dir order.
- **Aborted-session replay refused.** Replaying an aborted or crashed
  source session now returns a typed `SessionAborted` error instead of
  silently producing a session that "replayed" an abandoned trace.
- **`import.playwright` RPC.** Wires the existing `PlaywrightImporter`
  through `CoreFacadeBridge`, `CoreServiceAdapter`, `RpcHandlers`, and
  the `request_router`. `loom import playwright trace.zip` now works
  end-to-end.
- **Schema-aware CLI coercion.** `loom action web.type --text 5551234`
  no longer fails with "5551234 is not of type 'string'" — the parser
  consults the action's request schema and coerces per declared type.
- **Module file naming.** Renamed every `<module>/interfaces.rs` to
  `<module>/<module>.rs`. The `interfaces.rs` convention was a Phase 5
  pipeline artifact; runtime files now match the module they live in.

### Fixed

- `web.wait` was a no-op — it dispatched a single `Runtime.evaluate`
  predicate but never checked the result, so missing selectors
  returned success in 1 tick. Now surfaces `wait_predicate_false`
  with `selector did not appear before timeout`.
- HTTP 4xx/5xx responses with `ERR_HTTP_RESPONSE_CODE_FAILURE` were
  classified as generic `network_error` losing the status code; now
  the host's typed-error builder prefers `status >= 400` events
  before `error_reason` events.
- `--budget network=0` and `wall_clock=0s` accepted silently (0
  internally meant "unlimited"); now rejected at parse time.
- Profile help text claimed `safe`, `default` were valid profiles; the
  daemon's allowlist is `safe`, `standard`, `full`. Help text fixed.
- `Date.now()` / `Math.random()` leaked real wall-clock + unseeded
  values when called via `web.evaluate` *before* any `web.navigate`.
  Root cause: chromium's bootstrap about:blank context never had the
  determinism script applied. Fixed by lazy-spawning the
  determinism-injected target on the first evaluate.

## Pre-1.0

Pre-1.0 history lives in the source code-pipeline repository:
https://github.com/WhoIsJohannes/code-pipeline (under `projects/loom/`).
