# Loom

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
cargo install --git https://github.com/J-Mentiora/loom --tag v1.0.0 loom-cli
loom postinstall
loom doctor
```

`--tag` is required: `loom postinstall` fetches `loom-daemon`, `loom-mcp`, and
`loom-shim-chromium` from the GitHub Release matching the installed crate
version, so the tag must point at an existing release. (Substitute the latest
release version for `v1.0.0`.)

### Manual download — pre-built tarball

```bash
curl -fsSL https://github.com/J-Mentiora/loom/releases/latest/download/loom-installer.sh | sh
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
git clone https://github.com/J-Mentiora/loom
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

```
web.navigate    — load a URL, follow redirects, capture DOM + screenshot
web.click       — querySelector + .click(), surfaces selector-miss as typed error
web.type        — focus + setValue + dispatch input/change events
web.select      — set <select>.value, dispatch change
web.hover       — dispatch mouseover events
web.scroll      — scrollBy on selector
web.wait        — Runtime.evaluate boolean predicate; typed error on false
web.evaluate    — Runtime.evaluate, returns canonical-JSON value (or 64KB+ blob ref)
web.screenshot  — Page.captureScreenshot (PNG)
web.snapshot    — DOM.getDocument, returns hash + content_ref
```

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

v1.0.0 — first stable release. Production users include Mentiora's
GA-driven software-generation pipeline (the harness loom was extracted
from). API is stable; breaking changes will bump the major version.

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

## License

Dual-licensed under MIT or Apache-2.0 at your option. See
[LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).

## Credits

Loom was extracted from Mentiora's
[code-pipeline](https://github.com/WhoIsJohannes/code-pipeline) project,
where it was generated and hardened across 23 rounds of GA-driven
testing. The source pipeline retains a pointer at `projects/loom/` and
consumes loom as a regular Cargo dependency. See
[CREDITS.md](CREDITS.md) for the extraction provenance.
