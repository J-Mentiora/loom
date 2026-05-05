# Contributing to loom

Thanks for your interest. Loom is small, opinionated, and bias-toward
narrow, well-scoped changes with a typed error mode for any new failure
path.

## Building

```bash
git clone https://github.com/mentiora-ai/loom
cd loom
rustup target add wasm32-wasip2
cargo build --workspace
```

The `loom-cli` build script produces the `loom_surface_web.wasm` artifact.
By default (release builds) it reads the committed
`loom-cli/vendor/loom_surface_web.wasm` — the `vendored-wasm` cargo feature
makes `cargo install --git ... loom-cli` work without `rustup target add
wasm32-wasip2`. To rebuild from source instead (for instance after editing
`loom-surface-web/`), use `cargo build --no-default-features --features
postinstall`, or regenerate the vendored bytes with `just vendor-wasm` and
commit the diff. CI's `vendored-wasm-check` job will fail PRs where the
vendored bytes drift from `loom-surface-web/`'s source.

`loom-host`'s build script reads the surface artifact's SHA-256 to embed
in `LOOM_SURFACE_WEB_SHA256`. Editing the surface crate without `touch
loom-surface-web/src/lib.rs` may leave a stale SHA in `loom-host`. When in
doubt: `cargo clean -p loom-host -p loom-surface-web`.

## Testing

```bash
cargo test --workspace -- --test-threads=1
```

`--test-threads=1` is load-bearing: a handful of tests use static `/tmp/`
paths and clobber each other under parallel execution. We accept this
trade-off because the alternative (per-test tempdirs) bloats the test
fixtures meaningfully and we'd rather keep the test bodies readable.

## Coding conventions

- **Typed errors.** A failure mode that doesn't surface as a typed
  `kind` on the wire is a bug. New fail-fast paths should return a
  specific `LoomErrorCode` variant; the wire layer's
  `error_translator` maps it to a stable wire kind.
- **No corner-cuts on validation.** Parse boundaries (CLI args, RPC
  params, session IDs, blob references) validate up-front and reject
  with an actionable message. Never bury a `unwrap_or_default()` over
  user input.
- **Determinism is sacred.** Every code path that consumes time or
  randomness inside the WASM guest goes through the determinism
  harness. Adding a new surface verb means threading seed/epoch_ms
  through to the WIT guest at spawn-target time.
- **One concern per crate.** If you find yourself adding a wasmtime
  dep to `loom-shared`, you're in the wrong crate. The crate seams
  are deliberately tight; cross them via path imports, not new deps.

## Pull requests

Small + focused beats large + comprehensive. A 10-line PR with a
regression test for a real bug lands fast; a 1000-line refactor with
no specific motivation gets a polite request to break it up.

Commit messages: imperative mood, 60-char subject line, body explains
the *why*. Reference the failure case empirically:

```
fix(loom-core): block path-traversal session_ids that exfiltrate file content

A malicious session_id like "../evil" resolves outside sessions_root
because Path::join doesn't normalise ".." segments. fs::read parses
the planted manifest WAL and surfaces canonical_bytes as receipt
entries, leaking arbitrary file content.

Fix: is_valid_session_id gate before any path join. Wired into all
five core_api_facade paths (inspect, validate, replay, diff, export).

Verified end-to-end: inspect '../evil' → empty result (no fs::read
attempted); validate/replay/diff/export → typed SessionNotFound.
1189 (+27) tests green.
```

## Releases

Releases are cut from the `main` branch via git tag. `cargo-dist`'s
GitHub Actions workflow handles cross-compilation for macOS arm64/x86
and linux x86/arm64, uploads the binaries to the GitHub release, and
auto-publishes the Homebrew formula to the
[mentiora-ai/homebrew-loom](https://github.com/mentiora-ai/homebrew-loom) tap.

### One-time setup (first release only)

1. Create the tap repo `mentiora-ai/homebrew-loom` (the `homebrew-` prefix
   is required so `brew install mentiora-ai/loom/loom` resolves). Initialize
   with a single-line README.
2. Generate a **fine-grained** GitHub personal access token, scoped to
   only `mentiora-ai/homebrew-loom`, with `contents: write`. Do NOT use a
   classic `repo`-scoped PAT — that grants access to all your private
   repos and would fail an audit.
3. Add the PAT as a secret named `HOMEBREW_TAP_TOKEN` in the loom repo
   (Settings → Secrets and variables → Actions → New repository secret).
4. (Optional, recommended): bump cargo-dist to the latest release and
   regenerate the workflow:
   ```bash
   cargo install cargo-dist --locked
   dist init --yes  # picks up the homebrew installer + tap from dist-workspace.toml
   ```
   Commit any changes to `.github/workflows/release.yml` and
   `dist-workspace.toml`.

### Cutting a release

```bash
# 1. Bump the workspace version + add a CHANGELOG entry
#    (cargo-dist will refuse to release if these are out of sync)
# 2. Regenerate the vendored WASM if loom-surface-web/ changed:
just vendor-wasm
git add loom-cli/vendor/loom_surface_web.wasm
# 3. Tag + push
git tag -a v1.x.y -m "release v1.x.y"
git push origin v1.x.y
```

The release workflow takes ~10-15 minutes. After completion the
`homebrew-formula-smoke-check` job verifies the auto-pushed formula's
version matches the tag — failures here usually mean
`HOMEBREW_TAP_TOKEN` expired and needs rotation.

## Code of conduct

See [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
