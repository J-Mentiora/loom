# Cut loom release `<TAG>` — working checklist

> This is a COPY of `.claude/skills/cut-release/checklist.md`. Tick the boxes here
> (`[ ]` → `[x]`) with a one-line evidence note, and save after every step. Bumping
> `<OLD>` → `<NEW>`; tag `<TAG>`.

**MANDATORY STOPs:** Step 1 (version), Step 7 (merge), Step 8 (tag push — publishes to
public registries). Never tag without explicit user confirmation.

---

## Step 0: Pre-flight
- [ ] `git checkout main && git pull --ff-only` — main is current and clean. If main has
  uncommitted changes, STOP and stash them (don't clobber).
- [ ] CI on `main` is green (`gh run list --branch main --limit 1`).
- [ ] Create a release worktree:
  `git worktree add ../loom-release-<TAG> -b release-<TAG> main`. Work there.

Evidence: main_head= | ci=

---

## Step 1: Decide the version — MANDATORY STOP
- [ ] Review what landed since the last tag: `git tag -l | sort -V`, then
  `git log --oneline $(git describe --tags --abbrev=0)..main`.
- [ ] Choose `NEW` (patch = fixes/internal; minor = new user-facing capability). loom is
  pre-1.0 and **skips versions** — pick the number you want, not last-tag + 1.
- [ ] User confirmed `NEW` = `<NEW>` / `TAG` = `<TAG>`.

Evidence: changes_since_last_tag= | NEW=

---

## Step 2: Bump every version string (re-grep when done)
- [ ] `Cargo.toml` `[workspace.package].version = "<NEW>"` (members inherit — do NOT edit
  member crate Cargo.toml files).
- [ ] `Cargo.toml` `[workspace.dependencies]` internal pins (the 9
  `loom-* = { path=..., version="..." }`) → `<NEW>`, plus the comment above them.
  (cargo-deny holds these at `warn`, so a miss won't block — but bump in lockstep.)
- [ ] `README.md` — every non-ignored SemVer token → `<NEW>`:
  `grep -n '[0-9]\.[0-9]\.[0-9]' README.md`. Bump current-version + `--tag`/`pip install`
  lines. **Historical references** ("New in X", "Hardened in X") KEEP their old version
  and must carry a trailing `<!-- version-check-ignore -->` (add it if missing).
- [ ] `python-sdk/loom/__init__.py` `__version__ = "<NEW>"` (pyproject is dynamic — don't
  touch it).
- [ ] `typescript-sdk/package.json` `"version": "<NEW>"` AND `package-lock.json` (both
  `"version"` fields; `cd typescript-sdk && npm install` refreshes it).
- [ ] Re-grep: `grep -rn "<OLD>" Cargo.toml README.md python-sdk typescript-sdk` — only
  `version-check-ignore` historical lines may remain.

Evidence: files_bumped= | leftover_old_version=

---

## Step 3: CHANGELOG
- [ ] Insert under `## [Unreleased]`: `## [<NEW>] — <YYYY-MM-DD> — <Title>` (em-dash `—`),
  a summary paragraph, then `### Fixed` / `### Added` / `### Changed`. Reference the PRs
  since the last release. Never edit an already-published entry.

Evidence: changelog_entry_added=

---

## Step 4: Regenerate generated artifacts (only if their source changed this cycle)
- [ ] If `loom-rpc/src/action_registry/` changed since the last release:
  `cargo run --example gen-docs -p loom-cli`; commit `docs/actions.md`, `docs/loom.1`,
  `docs/loom-action.1`. (CI `gen-docs` job blocks a stale `docs/`.) Else: skip.
- [ ] If `loom-surface-web/` changed: `just vendor-wasm`; commit
  `loom-cli/vendor/loom_surface_web.wasm`. **Cross-host gotcha:** macOS-built wasm won't
  byte-match Linux CI — if `vendored-wasm staleness` fails on the PR, download that job's
  `vendored-wasm-fresh` artifact and commit those Linux bytes. Else: skip.
- [ ] Refresh `Cargo.lock`: `cargo build --workspace`; commit `Cargo.lock`.

Evidence: gen_docs= | vendor_wasm= | cargo_lock=

---

## Step 5: Verify locally
- [ ] `scripts/check-readme-version.sh "<NEW>" README.md` exits 0.
- [ ] `cargo build --workspace` clean; `cargo fmt --check`; `cargo clippy --workspace`.
- [ ] (Optional) `cargo test --workspace -- --test-threads=1`.

Evidence: readme_check= | build= | clippy= | fmt=

---

## Step 6: Open the release PR
- [ ] Commit (`release(<TAG>): bump version + changelog`), push, `gh pr create` → base
  `main`.
- [ ] `gh pr checks <PR> --watch` green. Required incl.: fmt, clippy, test
  (macos+ubuntu, stable+beta), build, smoke, e2e, **readme-version**, **gen-docs**,
  cargo-deny, **vendored-wasm staleness**, python-sdk + typescript-sdk tests, release.yml
  (plan mode), PyPI PR-validation (twine check, no publish). Fix red, re-push.

Evidence: pr_url= | ci=

---

## Step 7: Merge — MANDATORY STOP
- [ ] User confirmed merge. `gh pr merge <PR> --squash --delete-branch`.
- [ ] **Worktree quirk:** if `--delete-branch` errors `'main' is already used by worktree`,
  the squash merge STILL landed — verify `gh pr view <PR> --json state` == `MERGED`, then
  `git push origin --delete release-<TAG>` manually.
- [ ] `git checkout main && git pull --ff-only`; release commit is HEAD; CI on main green.

Evidence: merged_commit= | main_ci=

---

## Step 8: Tag + publish — MANDATORY STOP (irreversible, public)
- [ ] Required secrets exist (ask the user if unsure): `HOMEBREW_TAP_TOKEN`,
  `PYPI_TOKEN`, `NPM_TOKEN` (optionally pre-check via the manual `Verify NPM_TOKEN`
  workflow: `gh workflow run verify-npm-token.yml`).
- [ ] **User confirmed: push the tag and publish.** Then from updated `main`, create an
  **annotated** tag whose message is the release summary (loom tags carry full notes):
  ```bash
  git tag -a <TAG> -m "<TAG> — <Title>

  <2–4 line summary of the release; reference key PRs>"
  git push origin <TAG>
  ```
- [ ] Watch the three workflows:
  `gh run list --workflow release.yml --limit 1`,
  `gh run list --workflow publish-pypi.yml --limit 1`,
  `gh run list --workflow publish-npm.yml --limit 1`.

What fires on the tag:
- **release.yml** (cargo-dist): cross-compiles aarch64/x86_64 × macOS/Linux, builds
  shell + Homebrew installers, **creates the GitHub Release** (notes from CHANGELOG), and
  **pushes `loom.rb`** to `mentiora-ai/homebrew-loom`. ~10–15 min.
- **publish-pypi.yml:** verifies wheel version == tag, publishes `mentiora-loom`.
- **publish-npm.yml:** verifies `package.json` version == tag, publishes
  `@mentiora-ai/loom-sdk`.
- An SDK job failure does NOT block the binary release; fix + re-run from the Actions UI.

Evidence: tag_pushed= | release_run= | pypi_run= | npm_run=

---

## Step 9: Post-release verify + cleanup
- [ ] `gh release view <TAG>` — release exists with the 4 target artifacts + installers.
- [ ] PyPI shows `<NEW>` (`pip index versions mentiora-loom`).
- [ ] npm shows `<NEW>` (`npm view @mentiora-ai/loom-sdk version`).
- [ ] Homebrew formula in `mentiora-ai/homebrew-loom` bumped to `<NEW>`.
- [ ] `git worktree remove ../loom-release-<TAG>`.

Evidence: gh_release= | pypi= | npm= | homebrew=

---

## Gotchas (from real prior releases)
1. **SDK version drift = #1 failure.** v0.9.8's tag was pushed with SDK manifests still at
   0.9.4 → npm/PyPI tag-match gates failed → fix commit + re-tag. Always complete Step 2
   before Step 8.
2. **`readme-version` flags EVERY SemVer token** in README.md; one missed line blocks the
   PR. `<!-- version-check-ignore -->` is only for genuine historical references.
3. **No crates.io** — never `cargo publish` (`loom-cli/Cargo.toml` `publish = false`).
4. **Versions skip** — 0.9.4/5/6 were never tagged.
5. **Worktree merge quirk** — `--delete-branch` errors but the merge lands (Step 7).
6. **Homebrew publish failure** ≈ expired `HOMEBREW_TAP_TOKEN` (no formula smoke-check job
   exists despite the stale CONTRIBUTING.md note).
7. **Annotated tags carry full release notes** — `git tag -a` with a real summary, not a
   bare `-m "release"`.
