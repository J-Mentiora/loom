# Loom E2E Findings

End-to-end runs of every README-promised feature against local fixtures and
real public sites (saucedemo, the-internet, booking.com, wikipedia,
example.com). All test runner scripts live under `tests/e2e/`.

## Score

| Suite                               | Pass / Total | Notes |
|-------------------------------------|--------------|-------|
| `run_e2e.sh` — full surface coverage | **33 / 33** | Every CLI verb + replay equality + typed errors + parallel + budgets + form flow + example.com |
| `run_real_world.sh` — public sites   | **11 / 12** | Saucedemo full e-commerce checkout, the-internet login, wikipedia, booking.com (1 flake on booking title async render) |
| `run_mcp.sh` — MCP stdio transport   | **8 / 9**    | initialize / tools/list / tools/call all work; one was a test-grep bug, now fixed |
| `run_load.sh` — concurrent sessions  | **40 / 40 actions** | 8 sessions × 5 evaluates, all clean, completed in 4.4s (≈110 ms/action under contention) |
| Final e2e regression after type/select fix | **33 / 33** | No regressions on local fixtures from the framework-aware type/select implementation |

## Bugs found and fixed in this run

### 1. `loom-daemon --help` and `--version` would hang or error opaquely
**Severity**: usability bug for anyone reading docs.
The daemon's `parse_args` didn't recognize `--help`/`--version`, so typing
either spawned a full daemon (hanging if no daemon running) or fell
through to the socket bind which failed with `AddressInUse` if one was.
**Fix**: short-circuit on `--help`/`--version` before the threat-model
check + socket bind. `loom-daemon/src/lib.rs` async_main + new `print_daemon_help`.

### 2. Budget kill returned generic `session_aborted`, not typed `budget-exceeded`
**Severity**: contract bug — README explicitly promises typed `budget-exceeded`.
When the wall-clock budget timer killed the session, subsequent action
dispatch hit the terminal-status reject at line 489 of the daemon, which
unconditionally returned `LoomErrorCode::SessionAborted`. The
`session.kill_reason` (which records `KillReason::BudgetExceeded { kind,
observed, limit }`) was set by the kill callback BEFORE status flipped,
but the dispatch never consulted it.
**Fix**: terminal-status branch now reads `kill_reason` and surfaces
`BudgetExceeded` when set, falling through to `SessionAborted` otherwise.
**Verification**: `loom action ...` after wall-clock kill now returns
`Error: budget_exceeded: action dispatch failed` (was `session_aborted: ...`).

### 3. `web.type` and `web.select` broke on every React/Vue/Angular app
**Severity**: critical for production-readiness.
Implementation set `el.value = text` directly via `Runtime.evaluate`,
which bypasses React's value-tracker. The DOM `.value` was set, but
React's internal state still saw the field as empty — every subsequent
form submit failed with framework-side validation errors ("this field
is required").
**Repro**: `loom action web.type` to fill saucedemo.com login → click
submit → `Epic sadface: Username is required`.
**Fix**: use the native prototype value setter
(`Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set`),
the same trick Playwright and `@testing-library/user-event` use, so
React's tracker observes the change. Also focuses the element first.
Same fix applied to `web.select` (`HTMLSelectElement` setter).
**Verification**: full saucedemo.com login → add-to-cart → checkout flow
now passes. The-internet.herokuapp.com login also passes.

### 4. Doc/wire field name drift (3 places)
**Severity**: SDK consumers writing against docs would hit dead fields.
The action_registry's `returns:` strings hand-write field names that
disagree with the actual `ActionReceipt` struct:

| Verb           | Docs claim       | Actual wire field         |
|----------------|------------------|---------------------------|
| `web.evaluate` | `value`          | `return_value_json` (string-encoded JSON) |
| `web.screenshot` | `screenshot_ref` | `screenshot_after_hash`   |
| `web.snapshot` | `content_ref`/`hash` | `dom_snapshot_hash`       |

**Fix**: rewrote the three `returns:` strings in
`loom-rpc/src/action_registry/action_registry.rs`. CI gate
(`gen-docs`) regenerates `docs/actions.md` from the registry, so they
stay in sync going forward.

## Verified working features

These are claims from the README that we ran against real binaries and
saw pass:

- **Hash-chain replay equality**: `loom session replay $SRC && loom session diff $SRC $REPLAY` → `field_diffs: 0`. Bit-equal manifest from a fresh replay session.
- **Typed errors over the wire**: dns_failure (with `chromium_error: net::ERR_NAME_NOT_RESOLVED`), http_status (with `status_code: 404`), wait_predicate_false (with `verb: "wait"`), url_blocked (`javascript:` scheme), connect_refused (separate from dns_failure), js_throw (with full exception), budget_exceeded (after fix).
- **MCP stdio transport**: `initialize`, `tools/list`, `tools/call` all work. Tools surface as `loom.web.navigate`, `loom.web.click`, etc. with proper JSON-Schema input shapes. No session_id plumbing required from the client.
- **Parallel sessions**: 4 concurrent sessions on the local fixture all complete cleanly, each loading the same page deterministically.
- **Time-travel inspect**: `loom session inspect $SID --at-action 1` returns the session state at that action.
- **Validate**: `loom session validate $SID` checks the manifest hash chain end-to-end.
- **Determinism harness**: `Math.random()`, `Date.now()` injected at session-create. Replay equality test confirms it.
- **URL allowlist**: `javascript:`, `data:`, `file:` schemes rejected with `kind: url_blocked` before any navigation attempt.
- **Real-world SPAs work** (after the React fix): saucedemo (React e-commerce), booking.com (loaded fine, captcha-free for plain navigate), the-internet (vanilla form), wikipedia.

## Known remaining flakes

- **Booking.com title sometimes empty**: their SPA renders title async via JS based on locale/cookies. Sometimes `document.title` is empty when we eval immediately after navigate. Workaround: wait for `body` content settle before reading title.

## Files changed

- `loom-daemon/src/lib.rs` — `--help`/`--version` short-circuit, budget kill detection, framework-aware `web.type` and `web.select`, updated unit test for the new type wire shape, new `print_daemon_help`.
- `loom-rpc/src/action_registry/action_registry.rs` — corrected `returns:` strings for `web.evaluate` / `web.screenshot` / `web.snapshot` to match the actual `ActionReceipt` field names.
- `docs/actions.md` and `docs/loom-action.1` — regenerated from the registry via `cargo run --example gen-docs -p loom-cli`.
- `tests/e2e/run_e2e.sh`, `tests/e2e/run_real_world.sh`, `tests/e2e/run_mcp.sh`, `tests/e2e/run_load.sh` — new test runner scripts.
- `tests/e2e/fixtures/index.html`, `tests/e2e/fixtures/checkout.html` — local fixture pages.

## Things still to do (deferred)

- Consider a `loom doctor` improvement that emits the man-pages-not-installed advisory to stderr only — currently it's stdout, which makes `loom doctor | jq` need a `2>/dev/null` to parse cleanly.
- The `web.click` Beta limitation in the README around DOM coordinate edge cases remains — programmatic `.click()` works for buttons, but won't simulate mouse-coordinate-aware interactions (drag, click-elsewhere-to-blur, etc.). Worth a follow-up using `Input.dispatchMouseEvent` once hit-test refinements land.
- Add a budget-test that verifies the typed `kind: "budget-exceeded"` error envelope (not just the string) — current e2e check accepts either since the CLI renders the typed code through the human-readable formatter, but the JSON receipt should surface `error: { kind: "budget-exceeded", detail: ... }`.
