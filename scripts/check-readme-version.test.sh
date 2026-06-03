#!/usr/bin/env bash
#
# Unit test for check-readme-version.sh. Feeds the checker fixture READMEs with
# matching, diverging, and ignored version tokens and asserts the exit code
# (FND-0011). No external deps — runs anywhere bash + grep exist.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
checker="$here/check-readme-version.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

fail=0

# run_case <name> <expected-exit> <version> <readme-contents>
run_case() {
  local name="$1" want="$2" version="$3" body="$4"
  local fixture="$tmp/readme.md"
  printf '%s\n' "$body" > "$fixture"
  set +e
  "$checker" "$version" "$fixture" >/dev/null 2>&1
  local got=$?
  set -e
  if [ "$got" -eq "$want" ]; then
    echo "ok   - $name (exit $got)"
  else
    echo "FAIL - $name: expected exit $want, got $got"
    fail=1
  fi
}

run_case "matching version passes" 0 "0.9.8" \
  "Install with --tag v0.9.8. loom is **0.9.8** — pre-1.0."

run_case "diverging v-prefixed token fails" 1 "0.9.8" \
  "Install with --tag v0.9.7 loom-cli"

run_case "diverging bare token fails" 1 "0.9.8" \
  "loom is **0.9.2** — pre-1.0."

run_case "ignored line is skipped" 0 "0.9.8" \
  "Hardened in 0.9.0 (typed errors). <!-- version-check-ignore -->"

run_case "ignore marker does not excuse other lines" 1 "0.9.8" \
  "$(printf 'Hardened in 0.9.0 <!-- version-check-ignore -->\nInstall --tag v0.9.7')"

run_case "no version tokens passes" 0 "0.9.8" \
  "This README mentions no versions at all."

run_case "non-semver numbers are ignored" 0 "0.9.8" \
  "Chromium pinned at version 132 (Playwright build 1153)."

# Usage / input errors → exit 2.
set +e
"$checker" "0.9.8" >/dev/null 2>&1; got=$?
set -e
[ "$got" -eq 2 ] && echo "ok   - wrong arg count → exit 2" || { echo "FAIL - wrong arg count: got $got"; fail=1; }

set +e
"$checker" "not-a-version" "$tmp/readme.md" >/dev/null 2>&1; got=$?
set -e
[ "$got" -eq 2 ] && echo "ok   - malformed expected version → exit 2" || { echo "FAIL - malformed version: got $got"; fail=1; }

set +e
"$checker" "0.9.8" "$tmp/does-not-exist.md" >/dev/null 2>&1; got=$?
set -e
[ "$got" -eq 2 ] && echo "ok   - missing readme → exit 2" || { echo "FAIL - missing readme: got $got"; fail=1; }

if [ "$fail" -ne 0 ]; then
  echo "SOME TESTS FAILED" >&2
  exit 1
fi
echo "all check-readme-version tests passed"
