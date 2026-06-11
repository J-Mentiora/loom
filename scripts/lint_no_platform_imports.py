#!/usr/bin/env python3
"""
lint_no_platform_imports.py

Scans loom-host and loom-core source trees for forbidden platform-specific
and browser-engine imports. Any match in non-test, non-comment source is a
build break.

Banned symbols (case-sensitive unless noted):
  OS-specific:   AppKit, Foundation, CoreFoundation, libsecret, APFS,
                 ScreenCaptureKit
  CDP/browser:   DevToolsProtocol, chromedriver, ChromeDriver,
                 firefox (case-insensitive), gecko (case-insensitive),
                 webkit (case-insensitive), safari (case-insensitive)

Exclusions:
  - Files whose path component contains "test" (test files)
  - Lines that start with // (Rust line comments)
  - Lines that start with # (Python/shell comments)
  - Files in target/ directory

Exit code 0 = clean, 1 = violations found or a SCAN_DIRS entry is missing
(a missing dir means the crate layout drifted and the lint would otherwise
silently scan zero files — the v0.9.3 regression).
"""

import re
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
LOOM_ROOT = SCRIPT_DIR.parent

# Directories to scan (relative to LOOM_ROOT). The workspace puts each
# crate at the repo root (not under `src/`), so the path is
# `<crate>/src`, not `src/<crate>/src` (which was the v0.9.3 bug per
# A-W8 / FND-0040 / D40 — silently scanning zero files).
#
# `loom-keychain` is deliberately NOT scanned: it's the one crate
# permitted to import `security-framework` / `secret-service` because
# it owns the platform abstraction. The keychain trait surface (which
# loom-host / loom-core depend on) is platform-agnostic; the platform-
# specific impls live in `loom-keychain/src/{macos,linux}.rs` behind
# `cfg(target_os = "…")` and are scoped via the `wrappers` allowlist
# in `deny.toml`.
SCAN_DIRS = [
    LOOM_ROOT / "loom-host" / "src",
    LOOM_ROOT / "loom-core" / "src",
]

# Exact-string banned terms (case-sensitive)
BANNED_EXACT = [
    "AppKit",
    "Foundation",
    "CoreFoundation",
    "libsecret",
    "APFS",
    "ScreenCaptureKit",
    "DevToolsProtocol",
    "chromedriver",
    "ChromeDriver",
]

# Case-insensitive banned terms
BANNED_INSENSITIVE = [
    "firefox",
    "gecko",
    "webkit",
    "safari",
]

# Build patterns
_EXACT_RE = re.compile("|".join(re.escape(t) for t in BANNED_EXACT))
_INSENSITIVE_RE = re.compile(
    "|".join(re.escape(t) for t in BANNED_INSENSITIVE), re.IGNORECASE
)


def is_test_file(path: Path) -> bool:
    """Skip files with 'test' in any path component."""
    return any("test" in part for part in path.parts)


def is_comment_line(line: str) -> bool:
    """Skip lines that are purely comment or doc-comment."""
    stripped = line.lstrip()
    return stripped.startswith("//") or stripped.startswith("#")


def scan_file(path: Path) -> list[tuple[int, str]]:
    """Return list of (lineno, line) for every banned match."""
    violations = []
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return violations

    for lineno, line in enumerate(text.splitlines(), start=1):
        if is_comment_line(line):
            continue
        if _EXACT_RE.search(line) or _INSENSITIVE_RE.search(line):
            violations.append((lineno, line.rstrip()))

    return violations


def main() -> int:
    all_violations: list[str] = []

    # A missing scan dir means SCAN_DIRS is stale (crate moved/renamed) —
    # the exact silently-scanning-zero-files failure mode that shipped
    # broken for all of v0.9.3 (A-W8 / FND-0040 / D40). Fail loud instead
    # of skipping.
    missing = [d for d in SCAN_DIRS if not d.is_dir()]
    if missing:
        print(
            "lint_no_platform_imports: SCAN_DIRS entry missing — the crate "
            "layout drifted; update SCAN_DIRS:",
            file=sys.stderr,
        )
        for d in missing:
            print(f"  {d}", file=sys.stderr)
        return 1

    for scan_root in SCAN_DIRS:
        for rs_file in sorted(scan_root.rglob("*.rs")):
            if "target" in rs_file.parts:
                continue
            if is_test_file(rs_file):
                continue
            violations = scan_file(rs_file)
            for lineno, line in violations:
                all_violations.append(f"{rs_file}:{lineno}: {line.strip()}")

    if all_violations:
        print(
            f"lint_no_platform_imports: {len(all_violations)} violation(s):",
            file=sys.stderr,
        )
        for v in all_violations:
            print(f"  {v}", file=sys.stderr)
        return 1

    print("lint_no_platform_imports: clean")
    return 0


if __name__ == "__main__":
    sys.exit(main())
