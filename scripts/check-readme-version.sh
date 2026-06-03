#!/usr/bin/env bash
#
# check-readme-version.sh — fail if any version string in the README diverges
# from the single source-of-truth version (the workspace `Cargo.toml` version).
#
# The symptom (README version strings lagging a release) is already fixed; this
# gate exists to prevent recurrence. It is the lightweight, standalone analogue
# of the `gen-docs` CI gate (fail-on-mismatch), but it does NOT extend gen-docs:
# gen-docs regenerates docs from code, whereas this compares prose against a
# known-good value.
#
# Usage:
#   check-readme-version.sh <expected-version> <readme-path>
#
#   <expected-version>  MAJOR.MINOR.PATCH, e.g. 0.9.8. CI derives this from the
#                       workspace Cargo.toml and passes it in; keeping the value
#                       an *argument* (rather than parsing Cargo.toml here) makes
#                       this script pure and unit-testable with a fixture README
#                       (FND-0011).
#   <readme-path>       Path to the README to scan.
#
# A line may opt out of the check by containing the marker:
#
#   <!-- version-check-ignore -->
#
# Use it for deliberate historical references (e.g. "Hardened in 0.9.0"), where
# the version named is *not* meant to track the current release. The marker is
# an HTML comment, so it is invisible in the rendered README.
#
# Exit codes:
#   0  all version tokens match the expected version (or are explicitly ignored)
#   1  at least one version token diverges
#   2  usage / input error
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <expected-version> <readme-path>" >&2
  exit 2
fi

expected="$1"
readme="$2"

if [ ! -f "$readme" ]; then
  echo "error: README not found: $readme" >&2
  exit 2
fi

# Fail loud if the caller handed us a malformed version (e.g. a botched
# Cargo.toml parse) rather than silently matching nothing.
if ! printf '%s' "$expected" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
  echo "error: expected version '$expected' is not MAJOR.MINOR.PATCH" >&2
  exit 2
fi

marker='<!-- version-check-ignore -->'
status=0

# Walk line by line so we can (a) honour the per-line opt-out marker and
# (b) report precise line numbers. Tokens are v?MAJOR.MINOR.PATCH; a leading
# "v" is stripped before comparison so `v0.9.8` and `0.9.8` are equivalent.
lineno=0
while IFS= read -r line || [ -n "$line" ]; do
  lineno=$((lineno + 1))
  case "$line" in
    *"$marker"*) continue ;;
  esac
  for tok in $(printf '%s\n' "$line" | grep -oE 'v?[0-9]+\.[0-9]+\.[0-9]+' || true); do
    norm="${tok#v}"
    if [ "$norm" != "$expected" ]; then
      echo "::error file=$readme,line=$lineno::README version '$tok' diverges from source-of-truth '$expected' (workspace Cargo.toml). Update it, or append '$marker' to this line if the reference is intentionally historical."
      echo "  $readme:$lineno: $line" >&2
      status=1
    fi
  done
done < "$readme"

if [ "$status" -eq 0 ]; then
  echo "OK: all README version tokens match $expected (or are explicitly ignored)."
fi
exit "$status"
