# Council Code Review — Summary (Step 18)

5 reviewers via `council/review.py` against the post-implementation diff
(passed `council-plan-review-summary.md` as `--context` for continuity).

| Role | Verdict | Notes |
|---|---|---|
| security | APPROVE_WITH_CONDITIONS | 1 hallucinated finding (#1: claimed redaction skips curated renderers — wrong; the dispatcher pre-redacts BEFORE calling renderers in `curated/mod.rs::dispatch`). 4 Defensive Paranoia false positives (truncate panic on max=0 — internal callsite uses constant 26/10/20; web_evaluate panic on JSON serialization — total operation; warn_once mutex panic — single-threaded CLI; IsTerminal validation — infallible API). All dismissed. |
| code_quality | APPROVE_WITH_CONDITIONS | All findings were stale-branch artifacts: the diff at review time included "removals" of upstream commits (#5 cargo-dist + brew, #2 MCP fix, #9 tap fix) because branch base was f5c276c. Rebased onto current origin/main; diff now clean. The reviewer's own "Prior review context" section confirms D-29..D-34 all addressed. |
| test_engineer | APPROVE_WITH_CONDITIONS | 1 stale-branch hallucination (chromium download error path — upstream removal, not this PR's). 1 REAL finding adopted: redaction list missed `authorization`, `authentication`, `auth_header`. Added `auth_` (trailing-underscore precision-cut), `authoriz`, `authentic` patterns. 4 Defensive Paranoia false positives (env-var race condition in single-threaded CLI startup; ANSI sequence correctness — already pinned by golden fixtures; missing edge case in web_navigate — already handled by `if let Some(ns)`; no test for empty-quiet stdout — already covered by `quiet_session_list_empty_prints_nothing`). |
| devil | REQUEST_CHANGES (verdict downgrade after stale-branch artifacts) | ALL 4 findings were stale-branch hallucinations (cargo-dist downgrade, brew removal, WASM target dep, MCP content-type regression — all upstream commits, not this PR). The "D-30 not addressed" line is also wrong; the curated/ directory clearly exists with one file per renderer. After rebase, none of the findings apply. |
| process_critic | APPROVE | 1 REAL finding adopted: manual renderer registry creates a "forget-me" step. Plan D-33 specified the test but I missed implementing it. Added `tests/integration_method_registry.rs` enumerating every method emitted by handler code and asserting each maps to either a curated renderer or `SILENT_BY_DESIGN` allow-list. 1 minor finding declined (release.yml downgrade rationale comment — that's not in this PR's diff). |

## Net real fixes applied

1. **Redaction patterns**: added `auth_` / `authoriz` / `authentic` to `SENSITIVE_PATTERN_LOWERCASE`. New tests in `redact.rs::tests::expanded_patterns_all_match` cover `authorization`, `Authorization`, `proxy_authorization`, `authentication`, `auth_header`. Documented intentional over-match for `authentic_user_count` (safer to redact).
2. **Registry-coverage test**: new `tests/integration_method_registry.rs` enumerates 25 methods emitted by handler code; asserts each is in `curated::registry()` OR documented as silent. The `SILENT_BY_DESIGN` allow-list is currently empty — every receipt-emitting method has a curated renderer.

## Stale-branch artifact resolution

Before review, branch base was `f5c276c` while origin/main was at `4bffdd2` — 3 upstream commits ahead (PRs #5, #2, #9: release workflow, MCP fix, tap-org fix). The literal `git diff main..cli-pretty-tty` therefore showed the upstream commits as "reversions". After rebase onto current origin/main, the diff is clean and force-pushed. All "regression" findings from devil/code_quality/test_engineer are now N/A.

## Final status

- Tests: **410 / 0** (added 8 new — registry-coverage + expanded redaction patterns)
- All real findings addressed in commit `02bd19a` (review_fix).
- Verdict: **APPROVED** (substantive review; stale-branch artifacts resolved by rebase + 2 real fixes folded in).
