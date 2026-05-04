# Changelog

All notable changes to loom are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.0.0] — 2026-05-04

First stable release. Extracted from Mentiora's code-pipeline project
after 23 rounds of GA-driven hardening. API is stable; breaking changes
will bump the major version.

### Added

- **One-command install (AC-DIST-01..05).** Three install channels:
  Homebrew (`brew install J-Mentiora/loom/loom`), `cargo install --git`
  (with a `just install-loom` recipe that runs all four cargo-installs
  in one go), and pre-built tarballs from the GitHub release. The
  cargo-dist–generated release workflow builds for macOS arm64/x64 and
  Linux x64 (plus Linux arm64) on every `v*`-style tag and publishes
  tarballs + SHA-256 sums. Homebrew formulas are auto-pushed to
  `J-Mentiora/homebrew-loom`; the umbrella `loom` formula is
  hand-maintained at the same tap (depends_on the four cargo-dist-managed
  per-crate formulas) so the AC-DIST-03 user UX of a single `brew install`
  works.
- **System-Chromium PATH fallback (AC-DIST-05).** `loom session create`
  now searches `LOOM_CHROMIUM_PATH` → pinned `~/.config/loom/chromium/` →
  `$PATH` (chromium / chromium-browser / chrome / google-chrome) →
  macOS `/Applications/...`. Pinned still wins for replay-bit-equality
  (a `tracing::warn!` fires at boot when a non-pinned source is used).
  When no Chromium is found anywhere, the daemon returns
  `LoomErrorCode::BrowserNotFound` (wire `browser-not-found`); the CLI
  renders a platform-aware install hint (`brew install --cask chromium`
  on macOS, `apt install chromium-browser` / `dnf install chromium` /
  `pacman -S chromium` on Linux) instead of the previous opaque
  `shim-failure` exit-1.
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
