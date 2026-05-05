# Loom

[![CI](https://github.com/mentiora-ai/loom/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/mentiora-ai/loom/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/mentiora-ai/loom?include_prereleases&sort=semver)](https://github.com/mentiora-ai/loom/releases/latest)
[![License](https://img.shields.io/badge/license-Apache--2.0%20OR%20MIT-blue)](#license)
[![Rust](https://img.shields.io/badge/rust-1.92%2B-orange)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey)](#install)

**Agent-first browser automation runtime.** A local daemon + CLI + MCP server
that drives a real Chromium subprocess through a deterministic action store.
Designed for AI agents that need to browse, fill forms, run JavaScript, and
verify the result — with replay-equal hash chains, content-addressed
artifacts, and a typed error wire shape that doesn't leak generic
`internal_error` strings.

```
┌─────────────┐         ┌──────────────┐         ┌────────────────┐
│ loom (CLI)  │  ────►  │ loom-daemon  │  ────►  │ loom-shim-     │  ────► Chromium
│ loom-mcp    │  ◄────  │  (sessions,  │  ◄────  │  chromium      │
│ Claude/etc  │  JSON   │   replay,    │ CBOR    │  (CDP via WS)  │
└─────────────┘  RPC    │   manifest)  │         └────────────────┘
                        └──────────────┘
```

**Platforms.** macOS arm64/x86_64 and Linux x86_64/arm64. Windows is not supported on v0.9.0.

## What makes loom different

- **Deterministic replay.** Every action goes into a manifest WAL with a
  SHA-256 hash chain. `loom session replay <id>` reproduces the action
  chain bit-for-bit (excluding screenshot blobs, by design).
- **Typed errors over the wire.** `kind: "http_status"` with a real status
  code, `kind: "dns_failure"` with the chromium error name, `kind:
  "wait_predicate_false"` when a `web.wait` selector never appears — not
  a generic 500.
- **MCP-native.** `loom-mcp serve` exposes `loom.web.navigate`,
  `loom.web.click`, `loom.web.evaluate`, etc. as MCP tools with implicit
  session management — no `session_id` boilerplate in the client.
- **WASM-isolated surfaces.** The Chromium driver lives in a separate
  process; the WIT-based surface API is loaded as a wasmtime component,
  so a hostile page can't reach the host process directly.
- **Content-addressed everything.** DOM snapshots, screenshots, and
  exported tarballs all live in CAS keyed by SHA-256.

## Install

Pick whichever fits your environment. All three end at the same `loom postinstall`
step, which downloads + verifies the pinned Chromium build (~150 MB, one-time)
and AOT-compiles the WASM surfaces.

### Homebrew — macOS arm64/x64, Linux x64

```bash
brew install mentiora-ai/loom/loom
loom postinstall
loom doctor
```

### `cargo install` — any platform with Rust 1.92+

```bash
cargo install --git https://github.com/mentiora-ai/loom --tag v0.9.0 loom-cli
loom postinstall
loom doctor
```

`--tag` is required: `loom postinstall` fetches `loom-daemon`, `loom-mcp`, and
`loom-shim-chromium` from the GitHub Release matching the installed crate
version, so the tag must point at an existing release. (Substitute the latest
release version for `v0.9.0`.)

### Manual download — pre-built tarball

```bash
curl -fsSL https://github.com/mentiora-ai/loom/releases/latest/download/loom-cli-installer.sh | sh
loom postinstall
loom doctor
```

The installer drops all four binaries into `~/.cargo/bin` (or `~/.local/bin` if
Cargo isn't installed).

### After install: Gatekeeper on macOS

The release artifacts aren't notarized yet. On first run macOS may quarantine
`loom-shim-chromium`:

```bash
xattr -d com.apple.quarantine $(which loom-shim-chromium)
```

Notarization is tracked as a follow-up. Windows isn't supported — see
[Known limitations](#known-limitations) below.

### Build from source

```bash
git clone https://github.com/mentiora-ai/loom
cd loom
rustup target add wasm32-wasip2
cargo build --release
./target/release/loom postinstall
```

Source builds skip the vendored WASM artifact and compile the surface from
scratch, so they need the `wasm32-wasip2` target installed. The `cargo install`
path uses the vendored bytes and works without it.

## 5-minute quickstart

```bash
# Start the daemon (foreground; ^C to stop)
loom serve

# In another terminal: drive a real browser
SESSION=$(loom session create --profile standard | jq -r .session_id)
loom action web.navigate --session $SESSION -- --session $SESSION --url https://example.com
loom action web.evaluate --session $SESSION -- --session $SESSION --expression 'document.title'
loom session close $SESSION

# Inspect what just happened
loom session inspect $SESSION
loom session validate $SESSION   # PASS — hash chain + blob presence verified

# Replay it bit-for-bit
NEW=$(loom session replay $SESSION | jq -r .session_id)
loom session diff $SESSION $NEW   # field_diffs: []
```

## MCP server (Claude Desktop / Cursor / etc.)

Add to your MCP client config:

```json
{
  "mcpServers": {
    "loom": {
      "command": "loom-mcp",
      "args": ["serve"]
    }
  }
}
```

The server exposes the `loom.web.*` family. The client doesn't need to
know about session ids — `loom-mcp` lazily creates a session on first
tool call and reuses it across the conversation.

| Tool                  | Args                                  |
|-----------------------|---------------------------------------|
| `loom.web.navigate`   | `url: string`                         |
| `loom.web.click`      | `selector: string`                    |
| `loom.web.type`       | `selector: string, text: string`      |
| `loom.web.select`     | `selector: string, value: string`     |
| `loom.web.hover`      | `selector: string`                    |
| `loom.web.scroll`     | `selector: string, delta_y?: int`     |
| `loom.web.wait`       | `selector: string, timeout_ms?: int`  |
| `loom.web.evaluate`   | `expression: string`                  |
| `loom.web.screenshot` | `selector?: string`                   |
| `loom.web.snapshot`   | (no args)                             |

## Verbs

| Action          | What it does                                                            |
|-----------------|-------------------------------------------------------------------------|
| `web.click`     | Click an element by CSS selector.                                       |
| `web.evaluate`  | Run a JavaScript expression in the page and return the value.           |
| `web.hover`     | Dispatch a mouseover event at a CSS selector.                           |
| `web.navigate`  | Load a URL, follow redirects, capture DOM and screenshot.               |
| `web.screenshot`| Capture a PNG screenshot of the page or a selected element.             |
| `web.scroll`    | Scroll an element by a (delta_x, delta_y) offset.                       |
| `web.select`    | Set the value of a `<select>` element and dispatch `change`.            |
| `web.snapshot`  | Capture a full DOM snapshot of the active page.                         |
| `web.type`      | Focus an input and type text into it.                                   |
| `web.wait`      | Wait until a CSS selector resolves (or until timeout).                  |

Full per-action signatures (parameters, return shape, examples, typed
errors, profile guards) live in [docs/actions.md](docs/actions.md). At the
CLI you can also run `loom action --help` for the list and `loom action
<name> --help` for any single action's detailed signature. After
`loom postinstall` the same content is available offline as `man loom`
and `man loom-action`.

URL allowlist (for navigate): `http`, `https`, `about:blank`. Profiles:

- `safe` (default) — blocks `window.location` writes + similar
  destructive evaluate patterns; confines downloads.
- `standard` — no evaluate denylist; default download dir.
- `full` — no guards.

## Determinism

Every session has a `seed: u64` and an `epoch_ms: u64`, both fixed at
session-create time. Inside the page:

- `Math.random()` is sfc32 seeded from `seed`.
- `Date.now()`, `performance.now()`, `performance.timeOrigin` all
  return `epoch_ms`.
- `requestAnimationFrame` ticks at 16ms intervals (no real timing).
- All animations + transitions are 0s.

The determinism script is injected via `Page.addScriptToEvaluateOnNewDocument`
*and* explicitly run on the about:blank context, so `web.evaluate` works
deterministically even before the first navigate.

`loom session replay <id>` reuses the source session's `seed` + `epoch_ms`
+ `started_at_ms`, so the replay session's manifest hash chain is bit-equal
to the source's at every action_receipt entry.

## Security

- Path-traversal-safe session IDs (26 lowercase ASCII alphanumeric chars,
  ULID format). Anything else is rejected before any `fs::join`.
- WASM surface isolation: hostile JS in the page can't reach the host
  process — only the surface module's `host` interface is exposed, and
  it's a curated set (clock, RNG, blob_put/get, net_request, shim_call).
- `Browser.setDownloadBehavior(allowAndName, downloadPath=<session-dir>)`
  for safe-profile sessions, so downloads can't escape the session
  directory.
- Vault (OAuth token storage) requires explicit grants tied to a
  session ID + origin + scopes; tokens never enter the WASM guest's
  address space.

## Architecture

The workspace is 11 crates split along stable seams:

| Crate                | What lives here                                                |
|----------------------|----------------------------------------------------------------|
| `loom-shared`        | Wire-protocol types (CBOR), session IDs, shim_protocol         |
| `loom-keychain`      | Platform keychain access (macOS Keychain Services)             |
| `loom-core`          | Session state, manifest WAL, content store, vault, replay      |
| `loom-host`          | wasmtime runtime, WIT bindings, host_function_table            |
| `loom-rpc`           | JSON-RPC dispatch, schema validator, request router            |
| `loom-cli`           | `loom` binary — CLI commands + RPC client                      |
| `loom-daemon`        | `loom-daemon` binary — long-lived daemon that owns sessions    |
| `loom-mcp`           | `loom-mcp` binary — MCP server (stdio transport)               |
| `loom-surfaces`      | Cross-target surface verb implementations                      |
| `loom-shims`         | `loom-shim-chromium` binary — out-of-process Chromium driver   |
| `loom-surface-web`   | WASM cdylib — the `web.*` surface guest                        |

Each module's source file is named after the module
(`<module>/<module>.rs`). The `<module>/mod.rs` holds glue code;
`interface_tests.rs` holds tests.

## Status

loom is **0.9.0** — pre-1.0. The matrix below is the stability
contract: breaking changes to **Stable** rows bump the major version
when 1.0 ships; **Beta** rows may change without notice.

| Surface | Status | Notes |
|---|---|---|
| Receipt schema (`ActionReceipt`, `SessionManifest` wire format) | **Stable** | Hash chain + canonical bytes frozen. Breaking changes bump major. |
| Action / blob store (content-addressed SHA-256) | **Stable** | On-disk layout frozen; `loom gc` reference protection covers it. |
| Determinism harness (`Math.random`, `Date.now`, `performance.now`) | **Stable** | Seeded at session-create; reproduced bit-for-bit on replay. |
| Deterministic replay (manifest hash-chain bit-equality src ↔ replay) | **Beta** | Source/replay equality is not yet bulletproof — gated on real-Chromium subprocess wiring. |
| `web.navigate`, `web.evaluate`, `web.wait`, `web.type` | **Stable** | Covered by replay-equality tests. |
| `web.click` | **Beta** | DOM coordinate edge cases — gated on the hit-test refinements still in progress. |
| `loom-mcp` server (implicit session, tool surface) | **Stable** | Hardened in 0.9.0 (path-traversal-safe IDs, typed errors, lazy session). |
| CLI surface (`loom session`, `loom action`, `loom export`, `loom import`) | **Stable** | Flags pinned. `--version` format pinned: `loom <ver> (<sha> <date>)`. |
| `import.playwright` RPC | **Stable** | End-to-end wired through facade, adapter, handlers, router. |

**1.0 promotion criteria:** real-Chromium subprocess wiring + the `web.click` hit-test refinements land, matrix CI
green across the four release targets, no Beta rows remaining.

### Known limitations

- macOS arm64/x86 + linux x86/arm64 only. Windows isn't tested.
- macOS binaries are not notarized. First run may need
  `xattr -d com.apple.quarantine $(which loom-shim-chromium)` —
  see the install section above. Notarization is a follow-up.
- Chromium pinned at version 132 (Playwright build 1153). Newer
  Chromium revisions may require a `chromium_pin.rs` update.
- `loom-mcp`'s implicit session is single-session-per-process. Power
  users who need multiple parallel browsers per MCP connection should
  use the CLI directly.
- `loom postinstall` requires network access to fetch Chromium (and,
  for `cargo install` users, the auxiliary loom binaries). Air-gapped
  installs work via the manual-download tarball, which bundles all four
  binaries — only Chromium needs to be vendored separately on those hosts.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Short version: clone,
`cargo test --workspace -- --test-threads=1`, send a PR with a typed
error mode for any new failure path.

### Keeping docs in sync

The action surface (`web.click`, `web.evaluate`, …) is documented from a
canonical Rust registry at
[loom-rpc/src/action_registry/action_registry.rs](loom-rpc/src/action_registry/action_registry.rs).
Edits to the registry — adding actions, renaming params, expanding
descriptions — must be paired with regenerated docs:

```bash
just gen-docs              # or: cargo run --example gen-docs -p loom-cli
```

The CI gate fails any PR that desyncs `docs/actions.md` or the
`docs/loom*.1` man pages from the registry. A unit test in
[loom-rpc/src/action_registry/interface_tests.rs](loom-rpc/src/action_registry/interface_tests.rs)
also asserts the registry's required-param set equals the JSON-RPC
router's, so the registry and the dispatch path can't drift either.

## License

Dual-licensed under MIT or Apache-2.0 at your option. See
[LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).

## Credits

Loom was incubated inside a private Mentiora project and built with
substantial AI-assisted authoring (Anthropic Claude) under human review
at every gate. Stewardship by Johannes Rummel and the Mentiora team.
See [CREDITS.md](CREDITS.md) for more.
