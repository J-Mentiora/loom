# Changelog

All notable changes to loom are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **`SessionScope` primitive** ([`loom-core/src/session_scope/`](loom-core/src/session_scope/)).
  Per-session structured-concurrency parent: `tokio_util::sync::CancellationToken`
  paired with a `tokio::task::JoinSet`. All session-lifetime spawns become children;
  `drain(grace)` cancels cooperatively then force-aborts survivors. Replaces five
  fire-and-forget `tokio::spawn` sites that previously leaked `JoinHandle`s and
  saturated the daemon runtime after 4–6 sequential client sessions.
- **Per-request server-side deadline.** `LOOM_REQUEST_TIMEOUT_MS` (default
  `30000`) wraps `router.dispatch` so a hung shim or stuck dispatcher can't hold
  a connection task. Returns the typed `request-timeout` envelope on expiry.
- **`request.cancel` RPC.** Connection-scoped: cancels an in-flight request on
  the same connection by JSON-RPC `id`. Returns `{cancelled: bool}`. The
  cancelled request returns the typed `request-cancelled` envelope.
- **`session.kill` RPC.** Admin escape hatch for stuck sessions. Performs the
  abort flow plus blocks on shim teardown with a 5 s ceiling, then SIGKILL
  (`shutdown_process` already escalates SIGTERM(2s) → SIGKILL(1s) inside the
  ceiling).
- **`daemon.health` RPC.** Operational snapshot: `active_sessions`,
  `shim_breaker_states`, `otel_exporter` status. Shallow path is
  non-blocking; `{deep: true}` slot reserved for a follow-up shim probe.
- **`LoomErrorCode::RequestTimeout`** (wire string `request-timeout`) and
  **`LoomErrorCode::RequestCancelled`** (wire string `request-cancelled`).
  Both SemVer-minor additions per the existing wire-stability commitment.
- **Multi-session stress test** at
  [`loom-core/tests/multi_session_stall_repro.rs`](loom-core/tests/multi_session_stall_repro.rs)
  — 100 sequential session create+close cycles complete in under 1 s; the
  user-facing success criterion from the original investigation prompt.

### Changed

- **`ShimManager::record_failure`** circuit-breaker eviction no longer fires
  `tokio::spawn(shutdown_process(p))` fire-and-forget. Spawns into a
  `cleanup_tasks: JoinSet<()>` field with opportunistic `try_join_next`
  reaping on every `record_failure` and `shutdown_session`.
- **`CoreBridge::close_session_raw`** (daemon-side) similarly tracks
  `host.shutdown_session` background tasks in a `cleanup_tasks` JoinSet
  instead of leaking them via bare `tokio::spawn`.
- **`Session.budget_timer`** is now a SessionScope child with
  `tokio::select! { _ = cancel.cancelled() => {} _ = sleep(budget_ms) => ... }`
  so a closed session's timer exits cooperatively before its sleep
  completes, never tripping the kill callback after lifecycle exit.

### Removed

- **`Session.task_handle`** field — was dead code (declared but never
  populated). Replaced by the always-populated `Session.scope: Arc<SessionScope>`.
- **`Session.budget_timer`** field — superseded by the SessionScope-owned
  cancellation-aware timer.
- **`ReceiptMarshaller.queue_depth`** field + the docstring claiming
  `"Background queue depth 256; full → synchronous append on the calling task"`.
  The field was dead and the docstring lied — there was no bounded channel,
  no `try_send`, no backpressure. Removed both.

## [0.9.0] — 2026-05-05

Initial public pre-1.0 release. Loom incubated as an internal project before
this; see the README's [Status matrix](README.md#status) for which surfaces
are stable. Deterministic replay and `web.click` remain Beta — promotion to
1.0 requires those landing and the matrix CI green.

### Added

- **Determinism harness.** `Math.random()` (sfc32-seeded) and
  `Date.now()` / `performance.now()` (session-fixed epoch_ms) are pinned
  at session-create time and reproduced bit-for-bit on replay via the
  manifest's `started_at_ms`.
- **Implicit session management** in `loom-mcp serve`. MCP clients call
  `loom.web.navigate` with just `url` — the server lazily creates a
  session on first tool call and closes it on shutdown.
- **Path-traversal-safe session IDs.** Session paths reject anything
  that isn't a 26-char lowercase-alphanumeric ULID, preventing
  file-disclosure via `session.inspect '../evil'`.
- **Typed error mapping.** `kind: "http_status"` (with status code) for
  4xx/5xx responses, `kind: "wait_predicate_false"` for `web.wait`
  selector misses, `kind: "schema_violation"` for unsupported export
  formats with the actual supported list in the message.
- **Chromium subprocess crash detection.** The shim's supervisor spawns
  a task that `await`s `child.wait()`; the moment chromium dies, the
  shim self-exits → daemon's crash watcher fires → in-flight CDP
  commands fail in <1s instead of waiting the 30s recv timeout.
- **GC reference protection.** `collect_referenced_blobs` decodes
  `receipt_canonical_bytes` byte arrays and recursively scans for hex
  hashes, so `loom gc --ttl 0` no longer deletes blobs that active
  sessions still reference.
- **Stable session listing.** `loom session list` is now sorted by
  `created_at_ms` descending with `session_id` as tiebreak — independent
  of filesystem read_dir order.
- **Aborted-session replay refused.** Replaying an aborted or crashed
  source session now returns a typed `SessionAborted` error instead of
  silently producing a session that "replayed" an abandoned trace.
- **`import.playwright` RPC.** `loom import playwright trace.zip`
  imports a Playwright trace as a non-replayable Loom session.
- **Schema-aware CLI coercion.** `loom action web.type --text 5551234`
  no longer fails with `"5551234 is not of type 'string'"` — the parser
  consults the action's request schema and coerces per declared type.
- **Pretty TTY output.** Receipt-emitting subcommands auto-detect TTY
  and render colored multi-line layouts at a terminal; piped or
  redirected output stays canonical JSON, byte-for-byte identical to
  prior behaviour. New global flags: `--json`, `--pretty`, `--quiet` /
  `-q`, `--color={auto,always,never}` (with `--no-color` alias).
  Honours `NO_COLOR`, `CLICOLOR_FORCE`, `CLICOLOR=0`, and `TERM=dumb`.
  Per-stream color resolution lets a piped stdout still ship colored
  stderr error prose.
- **Recursive sensitive-field redaction in pretty output.** Receipt
  fields whose key (at any nesting depth) matches a conservative
  denylist (`token`, `secret`, `password`, `api_key`, `oauth`,
  `credential`, `cookie`, `jwt`, `session_key`, `access_token`,
  `refresh_token`, `private_key`, `signing_key`, `client_secret`, etc.)
  render as `<redacted>` in the human path. The `--json` path is
  unchanged — machines still see the raw value.
- **Curated multi-line layouts** for every receipt-emitting subcommand:
  `session create / inspect / list / close / abort / replay / diff /
  validate / export`, `action <surface>.<verb>` (all `web.*` verbs),
  `vault add / list / grant / revoke`, `gc`, `doctor`,
  `import playwright`, `benchmark`. Each renderer declares the keys it
  consumed; remaining receipt fields surface in a dim "more details"
  tail block in JSON-Schema property order.
- **Empty-state copy.** `session list` with no sessions prints
  `No sessions found.` instead of an empty header table; `vault list`
  empty prints `No vault entries.`.
- **README "Status" matrix** declaring stable vs beta surfaces, with
  concrete promotion criteria for 1.0.
- **`loom --version`** prints `loom 0.9.0 (<short-sha> <build-date>)`
  so issue reports identify the exact build. The short SHA + UTC build
  date are baked in by `loom-cli/build.rs` from `git rev-parse` at
  compile time, with a `"unknown"` fallback for source-tarball builds
  (no `.git` directory). `SOURCE_DATE_EPOCH` is honored for
  reproducible builds. The JSON-emitted `VersionInfo` carries
  `build_date` alongside the existing `version`, `git_sha`, `target`.

### Changed — breaking

- **`--pretty` semantics changed.** Previously, `--pretty` produced
  indented JSON via `serde_json::to_string_pretty`. It now produces
  the human-readable colored multi-line layout described above. If
  your scripts depend on the old indented-JSON shape, switch to
  `--json` (canonical single-line, machine-parseable) — that's what
  you want for `jq` and similar pipelines anyway.
- **`NO_COLOR` empty-string behaviour fixed.** Previously, setting
  `NO_COLOR=""` (empty) was treated as a disable signal, contrary to
  the [no-color.org](https://no-color.org/) spec. The variable now
  disables color only when set to a non-empty value. CI environments
  that set `NO_COLOR=""` for parent processes will no longer
  accidentally disable color.
- **Workspace version walked back from 1.0.0 → 0.9.0** to reflect that
  deterministic replay and `web.click` are not yet bulletproof.
  `v0.9.0` is the first supported release.

### Fixed

- `web.wait` was a no-op — it dispatched a single `Runtime.evaluate`
  predicate but never checked the result, so missing selectors
  returned success in 1 tick. Now surfaces `wait_predicate_false`
  with `selector did not appear before timeout`.
- HTTP 4xx/5xx responses with `ERR_HTTP_RESPONSE_CODE_FAILURE` were
  classified as generic `network_error` losing the status code. The
  host's typed-error builder now prefers `status >= 400` events
  before `error_reason` events.
- `--budget network=0` and `wall_clock=0s` accepted silently (0
  internally meant "unlimited"). Now rejected at parse time.
- Profile help text claimed `safe`, `default` were valid profiles; the
  daemon's allowlist is `safe`, `standard`, `full`. Help text fixed.
- `Date.now()` / `Math.random()` leaked real wall-clock + unseeded
  values when called via `web.evaluate` *before* any `web.navigate`.
  Root cause: chromium's bootstrap `about:blank` context never had the
  determinism script applied. Fixed by lazy-spawning the
  determinism-injected target on the first evaluate.

[Unreleased]: https://github.com/mentiora-ai/loom/compare/v0.9.0...HEAD
[0.9.0]: https://github.com/mentiora-ai/loom/releases/tag/v0.9.0
