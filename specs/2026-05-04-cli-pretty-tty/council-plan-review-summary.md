# Council Plan Review — Summary

5 reviewers via `council/review.py` against `plan.md` (post Gemini + ambiguity revisions).
Per-role transcripts in `council-plan-review.md/review_*.md`.

| Role | Verdict | Notes |
|---|---|---|
| security | **BLOCK** | Rubric mismatch — reviewer assumed multi-tenant SaaS w/ tenants & audit logging; we're a local single-user CLI for a sandboxed browser daemon. **Tenant isolation, audit logging, log-leakage frames N/A.** ONE legitimate point salvaged: extend D-18 redaction beyond the tail block. |
| code_quality | APPROVE_WITH_CONDITIONS | 3 conditions, all adopted (D-30, P3-2 cleanup, --quiet help text). |
| test_engineer | APPROVE_WITH_CONDITIONS | 7 issues — 4 adopted (determinism test, malformed-receipt fuzz, perf regression, expanded redaction regex). 3 deferred with rationale (concurrent TTY test = OS-level/not regression; terminal width matrix = D-11 already fixed at 80-col; full matrix test = subset adopted). |
| devil | APPROVE_WITH_CONDITIONS | 6 issues — 5 adopted (recursive redaction = D-29; --color conflict = D-31; warning rate-limit = D-32; method name mismatch test; new-command --quiet docs). 1 trimmed (compile-time `#[derive]` proc-macro is overkill — using runtime registry test instead). |
| process_critic | APPROVE_WITH_CONDITIONS | Methodological feedback ("plan is too heavy for a startup"). Acknowledged: this is the M-size workflow the user explicitly chose. Adopted: trim golden-test scope to happy paths only (D-34). |

## Final verdict

**APPROVE_WITH_CONDITIONS** — security BLOCK is rubric-driven, not substance. All substantive conditions captured as decisions D-29..D-34 below and folded into plan.md.

## New decisions from this round

- **D-29** Recursive sensitive-field redaction; applies to curated + tail + fallback paths uniformly. Expand regex.
- **D-30** Split renderers into `pretty_renderer/curated/` directory, one file per renderer.
- **D-31** `--color X --no-color` conflict → `validate_flags` rejects with Usage error.
- **D-32** Curated-renderer fallback stderr warning rate-limited: at most once per method per process (`OnceLock<HashSet<&'static str>>`).
- **D-33** Additional tests: tail-order determinism (loop ×100, byte-exact), malformed-receipt fuzz (missing fields, wrong types, deeply nested null), perf regression for 1000-item session.list (<100ms threshold), matrix of renderer-Err × stdout-tty/pipe × stderr-tty/pipe × flag combinations (focused subset, not full Cartesian).
- **D-34** Golden-file tests scoped to happy paths only (session.create, web.navigate, gc, doctor, error-with-color). Edge cases (empty list, redaction, tail block, malformed receipts) covered by unit tests with byte-shape assertions, not text fixtures. Reduces maintenance burden per process_critic feedback.

## Conditions explicitly NOT adopted (with rationale)

- **Tenant isolation / audit logging / log-leakage sanitisation** (security): N/A — loom-cli is a single-user local CLI; no tenants exist; daemon already runs under user's UID and writes to user's filesystem; no shared multi-tenant data model. Adopting these would be cargo-culting.
- **Concurrent multi-process TTY corruption test** (test_engineer): Not a regression — current canonical-JSON output has the same property (multi-process writes to same TTY can interleave). OS-level concern; we don't add new global locks.
- **Terminal width responsive layout** (test_engineer): D-11 explicitly defers responsive layout. 80-col floor is the v1 contract.
- **Compile-time `#[derive(CommandMethod)]` proc-macro for method-name validation** (devil): Overkill for a runtime-registry pattern; using a unit test that asserts every handler's `emit(method, ...)` first-arg matches a registry key is sufficient and cheaper.
- **Eliminate cross-model review** (process_critic): Acknowledged but the user explicitly chose M-size workflow. The cost-of-detail is intentional.
