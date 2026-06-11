#!/usr/bin/env bash
#
# Unit test for uninstall.sh. Runs the uninstaller against a throwaway $HOME so
# nothing on the real machine is touched, and asserts that --dry-run plans the
# right paths while deleting NOTHING (the load-bearing safety property for an
# rm -rf script). No external deps — runs anywhere bash exists.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
script="$here/uninstall.sh"
fail=0

note() { echo "ok   - $1"; }
bad()  { echo "FAIL - $1"; fail=1; }

# Build a fake install tree under a temp HOME so path resolution has something
# to find. Mirrors the real layout: CLI in ~/.cargo/bin, config dir, data dir.
fake_home="$(mktemp -d)"
trap 'rm -rf "$fake_home"' EXIT
mkdir -p "$fake_home/.cargo/bin" "$fake_home/.config/loom/chromium" \
         "$fake_home/.local/share/loom/bin" "$fake_home/Library/Application Support/loom/bin"
: > "$fake_home/.cargo/bin/loom"
: > "$fake_home/.config/loom/chromium/marker"

# Helper: run the script in the sandbox. HOME is overridden; XDG_DATA_HOME is
# cleared so the Linux data dir resolves to ~/.local/share/loom; PATH is
# trimmed so a `loom` installed on the test runner can't leak into the plan.
run() {
  env -i HOME="$fake_home" PATH="/usr/bin:/bin" bash "$script" "$@" 2>&1
}

# 1. --help exits 0 and prints usage.
if out="$(run --help)" && printf '%s' "$out" | grep -q "Usage:"; then
  note "--help prints usage and exits 0"
else
  bad "--help did not print usage / non-zero exit"
fi

# 2. unknown flag exits 2.
set +e
run --bogus >/dev/null 2>&1
code=$?
set -e
if [ "$code" -eq 2 ]; then note "unknown flag exits 2"; else bad "unknown flag: expected exit 2, got $code"; fi

# 3. --dry-run plans the binary, config dir, and data dir, and deletes nothing.
out="$(run --dry-run)"
for needle in "/.cargo/bin/loom" "/.config/loom"; do
  if printf '%s' "$out" | grep -q "$needle"; then
    note "--dry-run plan mentions $needle"
  else
    bad "--dry-run plan missing $needle"
  fi
done
if printf '%s' "$out" | grep -q "dry-run: nothing was removed"; then
  note "--dry-run announces no removal"
else
  bad "--dry-run did not announce no-removal"
fi

# The safety invariant: after --dry-run the fixture files are all still there.
if [ -f "$fake_home/.cargo/bin/loom" ] && [ -d "$fake_home/.config/loom" ]; then
  note "--dry-run deleted nothing"
else
  bad "--dry-run DELETED files — safety violation"
fi

# 4. A real removal with --yes clears the binary + config dir.
run --yes >/dev/null 2>&1
if [ ! -e "$fake_home/.cargo/bin/loom" ] && [ ! -e "$fake_home/.config/loom" ]; then
  note "--yes removed the binary and config dir"
else
  bad "--yes left files behind"
fi

# 5. A Homebrew-managed loom on PATH is never planned or deleted (the header's
#    "never touches a Homebrew prefix" contract). Fake brew layout: a Cellar
#    keg plus the $(brew --prefix)/bin symlink that `command -v loom` resolves.
brew_prefix="$fake_home/homebrew"
mkdir -p "$brew_prefix/Cellar/loom/0.0.0/bin" "$brew_prefix/bin"
printf '#!/bin/sh\n' > "$brew_prefix/Cellar/loom/0.0.0/bin/loom"
chmod +x "$brew_prefix/Cellar/loom/0.0.0/bin/loom"
ln -s "$brew_prefix/Cellar/loom/0.0.0/bin/loom" "$brew_prefix/bin/loom"
# Recreate a cargo-installed binary so the run still has something to remove.
mkdir -p "$fake_home/.cargo/bin"
: > "$fake_home/.cargo/bin/loom"
run_with_brew() {
  env -i HOME="$fake_home" PATH="$brew_prefix/bin:/usr/bin:/bin" bash "$script" "$@" 2>&1
}
out="$(run_with_brew --dry-run)"
if printf '%s' "$out" | grep -q "rm -rf $brew_prefix/bin/loom"; then
  bad "--dry-run plans rm -rf of the Homebrew binary — contract violation"
else
  note "--dry-run excludes the Homebrew binary from the removal plan"
fi
if printf '%s' "$out" | grep -q "brew uninstall loom"; then
  note "--dry-run plan points at brew uninstall for the Homebrew binary"
else
  bad "--dry-run plan does not mention brew uninstall"
fi
run_with_brew --yes >/dev/null 2>&1
if [ -e "$brew_prefix/bin/loom" ] && [ -e "$brew_prefix/Cellar/loom/0.0.0/bin/loom" ]; then
  note "--yes left the Homebrew binary intact"
else
  bad "--yes DELETED the Homebrew binary — contract violation"
fi
if [ ! -e "$fake_home/.cargo/bin/loom" ]; then
  note "--yes still removed the cargo-installed binary alongside the brew skip"
else
  bad "--yes failed to remove the cargo-installed binary"
fi

if [ "$fail" -ne 0 ]; then
  echo "uninstall.test.sh: FAILED"
  exit 1
fi
echo "uninstall.test.sh: all passed"
