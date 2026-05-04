# Changelog

All notable changes to loom are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Workspace version walked back from 1.0.0 → 0.9.0** to reflect that
  deterministic replay (gated on AC-SHCRT-08) and `web.click` (gated on
  AC-CLICK-*) are not yet bulletproof. The `v1.0.0` tag was published
  prematurely and remains on origin for historical reference, but
  0.9.0 supersedes it as the supported release. Promotion to 1.0
  requires both blockers landing and the matrix CI green.
- `loom --version` now prints `loom 0.9.0 (<short-sha> <build-date>)`
  so issue reports identify the exact build. The short SHA + UTC build
  date are baked in by `loom-cli/build.rs` from `git rev-parse` at
  compile time, with a `"unknown"` fallback for source-tarball builds
  (no `.git` directory). `SOURCE_DATE_EPOCH` is honored.

### Added

- README "Status" matrix declaring stable vs beta surfaces, with
  concrete promotion criteria for 1.0.
- `build_date` field on the JSON-emitted `VersionInfo` (alongside
  existing `version`, `git_sha`, `target`).

### Fixed

- `loom-shims` now sends `Runtime.enable` in the bootstrap script so
  console events fire end-to-end.

## [0.9.0] — 2026-05-04

Initial public pre-1.0 release. Extracted from Mentiora's code-pipeline
project after 23 rounds of GA-driven hardening. See the README's
[Status matrix](README.md#status) for which surfaces are stable;
deterministic replay and `web.click` remain Beta until AC-SHCRT-08 and
AC-CLICK-* land.

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

## Pre-0.9

Pre-0.9 history lives in the source code-pipeline repository:
https://github.com/WhoIsJohannes/code-pipeline (under `projects/loom/`).
