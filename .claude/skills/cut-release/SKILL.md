---
name: cut-release
description: >-
  Cut a new loom release (roll a version). Use when the user asks to "release",
  "roll a version", "cut vX.Y.Z", "publish a new version", or "bump the version".
  Drives the full checklist: version bump across Cargo/README/SDKs/CHANGELOG, a
  release PR, then an annotated tag push that triggers cargo-dist (GitHub Release +
  Homebrew) + PyPI (mentiora-loom) + npm (@mentiora-ai/loom-sdk). Tagging publishes
  to public registries and is irreversible — confirm with the user before pushing.
---

# Cut a loom release

loom ships from an **annotated git tag on `main`**. Pushing `vX.Y.Z` triggers three
parallel workflows that publish to **public registries** and create the GitHub
Release. There is **no `cargo publish`** — loom is not on crates.io
(`loom-cli/Cargo.toml` sets `publish = false`).

## How this skill works (READ FIRST)

This skill is **checklist-driven, like `/feature`**. Do NOT tick boxes in the
template that ships with this skill. Instead:

1. Establish the target version `NEW` (Step 1 of the checklist — ask the user).
2. **Copy the checklist template into a working file** and tick *that copy* as you
   go, saving after every step:
   ```bash
   DATE=$(date +%Y-%m-%d)
   SKILL_DIR=".claude/skills/cut-release"          # this skill's dir (repo-relative)
   WORK="specs/${DATE}-release-vNEW"               # specs/ is gitignored — local working state
   mkdir -p "$WORK"
   cp "$SKILL_DIR/checklist.md" "$WORK/checklist.md"
   # substitute the version placeholders in the copy:
   sed -i '' "s/<NEW>/NEW/g; s/<OLD>/<current-version>/g; s/<TAG>/vNEW/g" "$WORK/checklist.md"
   ```
3. **`$WORK/checklist.md` is now the instructions.** Work through it top to bottom,
   ticking each `[ ]` → `[x]` with a one-line evidence note, and re-saving the file
   after each step. One step at a time.

The template lives at `.claude/skills/cut-release/checklist.md`. It encodes the exact
files to edit, the CI gates that enforce them, the tag-trigger behavior, and the
hard-won gotchas (SDK version drift, the README version gate, the worktree-merge
quirk). Treat it as the source of truth and keep it updated when the release process
changes.

## Hard rules

- **MANDATORY STOP** (present and wait for the user) at three points: the version
  number (checklist Step 1), the merge gate (Step 7), and **before the tag push**
  (Step 8). The tag push publishes to PyPI/npm/Homebrew — never do it without explicit
  confirmation that the user wants to publish and that the secrets exist.
- Release only from a **clean, up-to-date `main`** on a dedicated branch + PR.
- The version bump must be **complete** — re-grep for the old version before pushing;
  the `readme-version` and SDK tag-match CI gates hard-fail a partial bump.
- Releases are **additive**: never rewrite a published CHANGELOG entry; never
  force-re-point a published tag.

## Authoritative facts (so the skill never guesses)

- **Version source of truth:** `Cargo.toml` `[workspace.package].version` (members
  inherit). Copies that CI enforces live in `README.md`, `python-sdk/loom/__init__.py`,
  `typescript-sdk/package.json` (+ `package-lock.json`), and the internal dep pins in
  `Cargo.toml [workspace.dependencies]`.
- **Release trigger:** `.github/workflows/release.yml` on tag glob
  `**[0-9]+.[0-9]+.[0-9]+*`; SDK publishes on `v*`. All fire on the same `vX.Y.Z` tag.
- **Publishes:** GitHub Release + Homebrew tap `mentiora-ai/homebrew-loom`
  (`HOMEBREW_TAP_TOKEN`), PyPI `mentiora-loom` (`PYPI_TOKEN`), npm
  `@mentiora-ai/loom-sdk` (`NPM_TOKEN`). NO crates.io.
- **History to learn from:** v0.9.8's tag was pushed before the SDK manifests were
  bumped → npm/PyPI publish failed → a fix commit + re-tag. The checklist's
  bump-SDKs-before-tag step exists to prevent exactly this. Versions also skip
  (0.9.4/5/6 were never tagged), so don't assume last-tag + 1.
- CONTRIBUTING.md has a `## Releases` section but it is **incomplete** (omits the SDK
  bumps; references a `homebrew-formula-smoke-check` job that does not exist). Trust
  this skill's checklist over CONTRIBUTING.md.
