# Security Policy

## Reporting a vulnerability

If you discover a security issue in loom, please **report it privately**.
The fastest path is GitHub's private vulnerability reporting:

- **Open a draft advisory:** https://github.com/mentiora-ai/loom/security/advisories/new

If you can't use GitHub Security Advisories, email **hi@mentiora.com** with
the subject prefix `[loom security]` and we'll triage from there.

Please **do not** file public GitHub issues for security vulnerabilities.

We aim to acknowledge reports within 2 working days, fix or document the
issue within 30 days for high-severity findings, and credit reporters in the
release notes once a patch ships (unless you ask to remain anonymous).

## Scope

In scope:

- The `loom`, `loom-daemon`, `loom-mcp`, and `loom-shim-chromium` binaries
  produced by this repository's release workflow
- The Rust crates published from this workspace
- The Homebrew formula at `mentiora-ai/homebrew-loom`
- The `loom-cli-installer.sh` script attached to GitHub Releases

Out of scope:

- Vulnerabilities in upstream dependencies (please report those upstream;
  see `Cargo.lock` for the resolved set)
- Issues that require a malicious local user with write access to the
  daemon's per-user data dir (`~/.local/share/loom` on Linux,
  `~/Library/Application Support/loom` on macOS) — that's the same trust
  boundary as `~/.ssh`
- Denial-of-service from operator-supplied scripts or browser pages

## Supported versions

| Version | Status                       |
|---------|------------------------------|
| 0.9.x   | Receiving security fixes     |
| < 0.9   | Not supported (pre-release)  |
