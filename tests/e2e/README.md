# tests/e2e/

End-to-end tests that drive the loom daemon and a real Chromium subprocess
against local fixture pages and (in some scripts) real public sites. They
exercise the README-promised features end-to-end: navigate, click, type,
wait, evaluate, screenshot, snapshot, replay equality, typed errors,
budgets, parallel sessions, and the MCP stdio transport.

## Scripts

| Script | What it covers | CI |
|---|---|---|
| `run_e2e.sh` | Every README-promised CLI verb against a local fixture; replay-equality bit-equal check; typed errors (`dns_failure`, `http_status`, `wait_predicate_false`, `url_blocked`); wall-clock budget enforcement; 4 parallel sessions; full form-checkout flow; example.com sanity. | every PR |
| `run_load.sh` | 8 concurrent sessions × 5 actions each, against the local fixture. Verifies the daemon doesn't crash under contention and per-action latency stays bounded. | every PR |
| `run_mcp.sh` | `loom-mcp serve` over stdio: `initialize`, `tools/list`, `tools/call` for navigate/click/evaluate/snapshot. | every PR |
| `run_real_world.sh` | Public sites: saucedemo (full e-commerce checkout), the-internet (login form), booking.com (search-only, no payment), wikipedia. | weekly cron + manual dispatch (external sites are inherently flaky for per-PR CI) |

## Running locally

```bash
# Build the binaries
cargo build --release

# Start the daemon (one terminal)
./target/release/loom serve

# Run the suites (another terminal)
bash tests/e2e/run_e2e.sh
bash tests/e2e/run_load.sh
bash tests/e2e/run_mcp.sh
bash tests/e2e/run_real_world.sh
```

Each script writes intermediate artefacts (manifests, raw error JSON,
per-session logs) to `tests/e2e/results/`. The directory is `.gitignore`d
since the contents change every run.

## Fixtures

`fixtures/index.html` and `fixtures/checkout.html` are deliberately
boring — plain HTML, vanilla JS, no framework — so replay equality can
hold and timing is deterministic. SauceDemo (React) and the-internet
(plain) cover the framework-aware code paths.

## CI integration

The `e2e` job in `.github/workflows/ci.yml` already downloads the pinned
Chromium build per OS. A follow-up step in that job invokes the three
PR-safe scripts. `run_real_world.sh` lives in a separate scheduled
workflow so a flake on booking.com's CDN doesn't block merges.

## Findings from the first run

[FINDINGS.md](FINDINGS.md) — the four bugs the initial e2e run surfaced,
with severity, repros, fixes, and verifications. Worth keeping around as
a record of what these tests were originally written to catch.
