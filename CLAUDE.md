# CLAUDE.md — working in the loom repo

loom is an agent-first browser-automation runtime: deterministic Chromium sessions
with replay-equal hash chains, MCP-native tools, and a content-addressed action
store. Rust workspace; SDKs in `python-sdk/` and `typescript-sdk/`.

## Skills

- **Cutting a release / rolling a version** → use the **`cut-release`** skill
  (`.claude/skills/cut-release/`). It is checklist-driven: it copies
  `checklist.md` into a local working file and ticks that copy through version bump
  → release PR → annotated tag → cargo-dist + PyPI + npm + Homebrew publish. Do NOT
  hand-roll a release; the skill encodes the gates and gotchas (SDK version drift,
  the README version gate, the worktree-merge quirk) that have bitten past releases.

## Build & test

- Build: `cargo build --workspace`.
- Test: `cargo test --workspace -- --test-threads=1` (single-thread is load-bearing —
  tests share `/tmp` fixtures).
- Per-crate: `cargo test -p loom-mcp` (etc.).
- Real-browser e2e is `#[ignore]`d and uses a fake-chromium harness:
  `cargo build -p loom-shims --features fake-chromium-bin --bin fake-chromium`, then
  `cargo test -p loom-host --test integration_shim_e2e -- --ignored`.
- Lint/format gates: `cargo clippy --workspace --all-targets`, `cargo fmt --check`.

## Generated artifacts (CI gates these — regenerate when their source changes)

- **Vendored WASM guest** `loom-cli/vendor/loom_surface_web.wasm` — rebuild after
  editing `loom-surface-web/` with `just vendor-wasm`. **Cross-host gotcha:** macOS
  builds do NOT byte-match Linux CI; if the `vendored-wasm staleness` job fails,
  download its uploaded `vendored-wasm-fresh` artifact and commit those Linux bytes.
- **Docs/man pages** `docs/actions.md`, `docs/loom.1`, `docs/loom-action.1` — rebuild
  after changing `loom-rpc/src/action_registry/` with `cargo run --example gen-docs -p
  loom-cli` (the `gen-docs` CI job blocks a stale `docs/`).
- **README version** must match `Cargo.toml [workspace.package].version` (the
  `readme-version` CI job; checker is `scripts/check-readme-version.sh`).

## Conventions & gotchas

- Workspace version is the single source of truth in `Cargo.toml [workspace.package]`;
  member crates inherit via `version.workspace = true`.
- loom is **not published to crates.io** (`loom-cli/Cargo.toml` `publish = false`) —
  never run `cargo publish`. Distribution is cargo-dist (binaries + Homebrew) + the two
  SDK registries.
- Determinism (NFR-DET-01): the manifest hash chain must stay replay-equal; screenshots
  are excluded from the chain (only their content hash is in the receipt). Don't break
  replay.
- PRs squash-merge to `main`. Releases are annotated tags on `main`.
- `specs/`, `council/`, and most of `.claude/` are gitignored (local working state);
  shared skills under `.claude/skills/` are tracked.

## Navigation

- `CONTRIBUTING.md` — contributor setup + a `## Releases` section (note: that section is
  incomplete re: SDK bumps — prefer the `cut-release` skill).
- `dist-workspace.toml` — cargo-dist config (targets, Homebrew tap, installers).
- `.github/workflows/` — `ci.yml`, `release.yml` (tag-driven), `publish-pypi.yml`,
  `publish-npm.yml`.
