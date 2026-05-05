# Pretty TTY Output — Phase 2: Design


Progress tracker for Steps 4–11. Tick boxes and fill evidence after EVERY step.
Save this file to disk after each step.

> **Slug**: `cli-pretty-tty` | **Branch**: `cli-pretty-tty` | **Started**: 2026-05-04
> **Spec folder**: `specs/2026-05-04-cli-pretty-tty/` | **Mode**: _(from discovery-output.md)_
> **Size**: _(from discovery-output.md)_ | **Domain preset**: _(from discovery-output.md)_

**RULES:** (1) MANDATORY STOP = present output, WAIT. (2) Questions via `AskUserQuestion`
with 2-4 options. (3) One step at a time. (4) BLOCKING = KILL, gate fail ×3, policy violation.
(5) Artifacts in `specs/2026-05-04-cli-pretty-tty/`. (6) Update `FEATURE_STATE.json` after each step.
(7) Log OpenRouter calls to `cost-log.jsonl`. (8) **Telemetry**: log step start/end to
`specs/2026-05-04-cli-pretty-tty/timings.jsonl` (`{"step": N, "phase": 2, "event": "start|end", "ts": "..."}`).

### Pre-read

- [ ] `specs/2026-05-04-cli-pretty-tty/discovery-output.md` read and context loaded

---

## Step 4: Web Research

> **SIZE GATE:** Skip for **S**. **MODE GATE:** BUILD only. FIX → skip to Step 7.

Run 2-3 targeted web searches. Use `WebSearch` for broad queries, Context7 for library docs.

For each search, record: **Query**, **Key findings** (2-3 bullets), **Relevance**,
**Libraries found** (name, health, license). Save to `specs/2026-05-04-cli-pretty-tty/research-notes.md`.

- [x] 3 web searches executed (TTY conventions; Rust ecosystem; --quiet semantics)
- [x] Context7 consulted (n/a — stdlib + ANSI is the whole surface)
- [x] Findings saved to `specs/2026-05-04-cli-pretty-tty/research-notes.md`

Evidence: queries=3 | libraries=anstyle (transitive via clap), is-terminal/atty deprecated → stdlib IsTerminal, owo-colors/nu-ansi-term/colored alternatives | key_insights=(1) atty deprecated; std::io::IsTerminal is the call. (2) NO_COLOR spec requires non-empty value — current pretty_renderer.rs:101 `is_ok()` check is buggy (empty string passes). (3) anstyle already transitive via clap; hand-rolling ANSI is fine but must be NO_COLOR-correct. (4) --quiet semantics split docker (id) vs ripgrep (silent); we picked id-style per Phase 1 Q3. (5) Stderr discipline: any banner/header/spinner MUST go to stderr — stdout reserved for receipt projection. (6) termcolor crate avoid (deprecated Windows API).

---

## Step 5: Design Intake Council

> **SIZE GATE:** S: skip council, ask 2-3 in-context "have you considered?" questions.
> M: run full council. L: full council + **MANDATORY STOP**.
> **MODE GATE:** BUILD only. FIX → skip to Step 7.

```bash
source .env.dev.local && export OPENROUTER_API_KEY && \
python3 council/design_intake.py specs/2026-05-04-cli-pretty-tty/discovery-output.md \
  --scan specs/2026-05-04-cli-pretty-tty/codebase-scan-light.md \
  --research specs/2026-05-04-cli-pretty-tty/research-notes.md \
  --output specs/2026-05-04-cli-pretty-tty/design-intake.md
```

Sends context to Gemini 2.5 Pro, o3, DeepSeek R1 in parallel. Each returns 3-5 probing
questions. Review and annotate all: 🔴 Critical / 🟡 Worth considering / ⚪ Low priority.
Present to user via `AskUserQuestion` (up to 4 per call).

- [x] Design intake ran (gemini-2.5-pro, o3, deepseek-r1 in parallel)
- [x] Output saved to `specs/2026-05-04-cli-pretty-tty/design-intake.md`
- [x] All questions presented with relevance annotations (4 critical asked via AskUserQuestion; 7 advisory defaulted in decisions.md)
- [x] User answers recorded in `specs/2026-05-04-cli-pretty-tty/decisions.md` (D-7 through D-17)
- [x] _(L only)_ N/A — size M

Evidence: questions_generated=14 (5+4+5) | presented=4 critical via AskUserQuestion | user_answers=all recommended | insights=flag precedence (--quiet>--json>--pretty>auto, mutual exclusion); universal --quiet rule (id/list-of-ids/silent); "more details" tail block; status-quo error format (color stderr at TTY only).

---

## Step 6: Clarifying Questions (Round 2, if needed)

> **SIZE GATE:** S: skip. M: 0-2 Qs if needed. L: ask if needed, own **MANDATORY STOP**.

Ask when: research revealed multiple approaches, intake raised scope-changing question,
findings conflict. Skip when: Steps 4-5 confirmed understanding, all intake Qs low-priority.

- [x] Questions asked (count): 0 — skipped, confident after Step 5 (4 critical questions resolved cleanly with no contradictions)
- [x] Answers: N/A

Evidence: questions=0 | answers=N/A | confidence=high (no conflicts in Step 5 answers, no scope re-opens)

---

## Step 7: Deep Codebase Scan

> **SIZE GATE:** S: this is their ONLY scan. M/L: second, deeper scan.

**ISOLATION RULE:** Run as `Agent` (`subagent_type: "Explore"`, `model: "opus"`,
thoroughness "very thorough").

Include key findings from Steps 4-6 in the agent prompt.

**BUILD mission**: Find reusable code, refactoring opportunities, cross-system impact,
duplication & friction, collision risks, integration points.

**FIX mission**: Find affected code paths, related tests, sibling patterns, recent changes,
blast radius.

- [x] Explore agent launched (subagent_type=Explore, "very thorough")
- [x] Report saved to `specs/2026-05-04-cli-pretty-tty/codebase-scan.md`
- [x] _(BUILD)_ Refactorings: 3 BLOCKERS surfaced — all addressed in plan v1: (1) `session inspect`/`session diff` field projection retained but emitted via the same `emit(method, projected, cfg)` signature; (2) `doctor` migrated through `emit`; (3) NO_COLOR spec bug fixed (D-16).
- [x] _(L only)_ N/A — size M

Evidence: scan_mode=BUILD/very thorough | findings_count=3 blockers + 5 traps + cross-system (loom-mcp consumer) | key_findings=field projection in inspect/diff (mitigation: emit accepts pre-projected value); doctor carve-out migrated; NO_COLOR fix planned.

---

## Step 8: Design

### BUILD mode: Design (ReAct Loop)

> **MODE GATE:** BUILD only — FIX mode section below.

Plan MUST incorporate findings from Steps 4-7.

### Loop: Clarify until confident

Repeat: **Think** (identify biggest uncertainty) → **Act** (ask ONE `AskUserQuestion`) →
**Observe** (record answer, re-assess). Exit when you could write a plan the user would
approve without major changes. Zero iterations is fine for well-defined tasks.

- [x] Iterations: 1 (Step 5 council intake yielded 4 critical questions; resolved in one round)
- [x] Confidence reached

### Reuse & framework check

1. **Public libraries** — evaluate candidates from Step 4 research
2. **Internal components** — revisit Step 7 scan for reusable/generalizable code
3. **Duplication risk** — cross-reference with scan findings

- [x] Libraries evaluated: stdlib `std::io::IsTerminal` (chosen, no new dep). Hand-rolled ANSI (chosen). anstyle (already transitive via clap; runner-up). Avoided: `atty` (deprecated), `termcolor` (deprecated Win API), `is-terminal` crate (unnecessary on MSRV 1.92).
- [x] Internal reuse identified: existing `PrettyRenderer` (schema-driven generic fallback), existing `OutputFormatter::write` API surface, existing `SchemaCache` for response schemas → tail block ordering.
- [x] Duplication risks flagged: existing `format_output(value, bool)` will be deleted, replaced by `emit(method, value, cfg)`. The schema-driven `PrettyRenderer::render` remains and is now actually reachable via the fallback path.
- [x] Ask via `AskUserQuestion`: covered by Step 3/Step 5 question rounds. No additional reuse questions surfaced.

### Draft the plan

- [x] Plan presented (WHAT, WHY, HOW)
- [x] Q&A recorded in `specs/2026-05-04-cli-pretty-tty/decisions.md` (D-1..D-28)
- [x] Plan drafted; items tagged `[AI_CODE]` (all). No `[HUMAN_ACTION]` required.
- [x] Plan saved to `specs/2026-05-04-cli-pretty-tty/plan.md`

### Ambiguity detection (M/L only)

> **SIZE GATE:** Skip for **S**.

```bash
source .env.dev.local && export OPENROUTER_API_KEY && \
python3 council/ambiguity_detection.py specs/2026-05-04-cli-pretty-tty/plan.md \
  --decisions specs/2026-05-04-cli-pretty-tty/decisions.md \
  --output specs/2026-05-04-cli-pretty-tty/ambiguity-report.md
```

- [x] Ambiguity scan ran (`council/ambiguity_detection.py` via GLM-4.7)
- [x] Findings addressed: 6 findings (3 critical, 3 advisory) — ALL FIXED. Critical: A1 deterministic tail order (D-24); A4 renderer-Err fallback (D-23); A5 emit() callsite error propagation (D-25). Advisory: A2 table column widths (D-26); A3 diff line format (D-27); A6 pluralization (D-28).

### Candidate exploration (L only)

> **SIZE GATE:** Skip for **S** and **M**.

⚡ **PARALLEL:** Launch simultaneously with ambiguity detection.

```bash
source .env.dev.local && export OPENROUTER_API_KEY && \
python3 council/candidate_exploration.py specs/2026-05-04-cli-pretty-tty/plan.md \
  --decisions specs/2026-05-04-cli-pretty-tty/decisions.md \
  --scan specs/2026-05-04-cli-pretty-tty/codebase-scan.md \
  --output specs/2026-05-04-cli-pretty-tty/candidates.md
```

- [ ] Candidates generated (or "skipped — S/M")
- [ ] Plan updated if needed

### Cross-model convergence loop (M/L only)

> **SIZE GATE:** Skip for **S**.

```bash
source .env.dev.local && export OPENROUTER_API_KEY && \
python3 council/cross_model_review.py specs/2026-05-04-cli-pretty-tty/plan.md \
  --decisions specs/2026-05-04-cli-pretty-tty/decisions.md \
  --prior specs/2026-05-04-cli-pretty-tty/gemini-review.md \
  --output specs/2026-05-04-cli-pretty-tty/gemini-review.md
```

Loop until convergence or max 3 iterations.

- [x] Iteration 1: gemini-2.5-pro critique → 6 findings, ALL incorporated as decisions D-18..D-23
- [x] _(Iteration 2)_: not run — every G/A finding is non-conflicting tightening (no redirection); plan converged in iteration 1
- [x] Converged (1 iteration, all findings incorporated)
- [ ] **MANDATORY STOP**: Plan presented to user (will combine with Step 11 council verdict per template)

Evidence (BUILD): question_rounds=3 (Steps 3, 5; Step 6/9 skipped — confident) | libraries=stdlib IsTerminal + hand-rolled ANSI | reuse_decisions=PrettyRenderer for fallback path; SchemaCache for tail order | plan_items=13 build steps (P3-1..P3-13) + 4 test files + ~6 new files + ~14 modified files | ambiguity=6/6 findings fixed | confidence=high

---

### FIX mode: Investigate & Fix

> **MODE GATE:** FIX only — skip for BUILD.

Use deep scan from Step 7 as starting point. Ask user at least one clarifying question
about the bug via `AskUserQuestion`.

Classify root cause: **Selector** / **Timing** / **Data** / **Logic** / **Integration**

### Hypothesis loop

For each: state hypothesis → gather evidence → verdict (CONFIRMED/REFUTED).
**3-strike rule:** 3 refuted → **MANDATORY STOP**, escalate.

Fix MUST be minimal.

- [ ] Clarifying question asked
- [ ] Root cause hypothesized
- [ ] Hypothesis 1: stated, evidence, verdict
- [ ] _(Hypothesis 2-3 if needed)_
- [ ] Root cause confirmed
- [ ] Fix applied — minimal, targeted
- [ ] `specs/2026-05-04-cli-pretty-tty/plan.md` + `specs/2026-05-04-cli-pretty-tty/decisions.md` written

Evidence (FIX): symptom= | category= | hypotheses= | root_cause= | files= | lines=

---

## Step 9: Clarifying Questions (Round 3, if needed)

> **SIZE GATE:** S: skip. M: 0-2 Qs if needed. L: ask if needed.

Ask when: plan reveals un-discussed implications, trade-offs need user input, research
contradicts plan. Skip when: plan follows naturally, no new trade-offs.

- [x] Questions asked: 0 — skipped, plan v2 has no new trade-offs surfaced. Defaults & deferrals are documented in decisions.md (D-11..D-17).
- [x] Answers: N/A

---

## Step 10: UI Mockups

> **CATEGORY GATE:** frontend or fullstack only. Skip backend, infra, tooling.
> **SIZE GATE:** Skip for **S**. **MODE GATE:** BUILD only.

Describe UI implications per page/view. Present via `AskUserQuestion`:
A) Skip mockups  B) Text descriptions only  C) Generate specific  D) Generate all

If generating: use `specs/_templates/mockup-boilerplate.html` as base. Sub-agents fill
content. Output to `specs/2026-05-04-cli-pretty-tty/mockups/`. Open for user review.

**Post-review (MANDATORY STOP)**: A) Approve  B) Request changes  C) Return to design  D) Reject

- [x] **SKIPPED — CATEGORY GATE**: tooling category (CLI). No UI mockups required. No browser-rendered surface; the "UI" is multi-line stdout text whose layout is fully specified in plan.md P3-6 (per-command sections).

---

## Step 11: Council Review (Plan) + Test Spec — Red (TDD)

> **SIZE GATE:** S: skip council, go to test spec.

**ISOLATION RULE:** Reviewers MUST be independent sub-agents or external LLM calls.

### Role selection

Always: `security`, `code_quality`, `test_engineer`, `devil`. Add from domain preset +
feature-specific roles (end_user for UI, performance for DB/API, devops for migrations, etc).

```bash
source .env.dev.local && export OPENROUTER_API_KEY && \
python3 council/review.py specs/2026-05-04-cli-pretty-tty/plan.md \
  --roles security,code_quality,test_engineer,devil,<additional> \
  --rubric plan
```

⚡ **PARALLEL:** Launch council in background, immediately write test spec.

- [x] Roles selected: security, code_quality, test_engineer, devil, process_critic. (security/code_quality/test_engineer/devil = always-required core; process_critic = `tooling` domain preset.)
- [x] Council launched in background via `council/review.py`
- [x] No reviewers simulated in-context (real per-role calls, transcripts in `council-plan-review.md/`)
- [x] Output saved to `specs/2026-05-04-cli-pretty-tty/council-plan-review.md/` (directory of per-role files) + `council-plan-review-summary.md` (aggregated)
- [x] BLOCK from security analysed: rubric mismatch (assumed multi-tenant SaaS; loom-cli is single-user local CLI). Salvaged ONE substantive point — extend redaction across all paths (folded into D-29). Other security points (tenant isolation, audit logging, log leakage) explicitly N/A and documented in summary.
- [x] Final verdict: **APPROVE_WITH_CONDITIONS**. All conditions captured in D-29..D-34 and folded into plan v3.

### Write the test spec

Write spec in `frontend/agentic-tests/specs/`. See `frontend/agentic-tests/README.md`.

Exceptions: `EXCEPTION: pure-backend` / `EXCEPTION: flag-off` / `EXCEPTION: hotfix`

- [x] Test spec: **EXCEPTION: pure-backend** — feature touches only `loom-cli/` (Rust binary). No frontend/agentic-tests applicable. Test plan lives in plan.md P3-8 through P3-11c (8 dedicated test files).
- [x] Spec copy: N/A (exception applies)

### Verify Red

```bash
cd frontend && npm run test:agentic -- --spec specs/<spec-file>
```

- [x] Test Red verification: N/A under EXCEPTION: pure-backend. Phase 3 will follow TDD by writing the failing tests first (P3-8 through P3-11c), running `cargo test` to confirm they fail, then implementing.

### Combined MANDATORY STOP

Present council verdict + test spec together.

- [ ] **MANDATORY STOP**: Both plan + council verdict presented to user; awaiting approval to proceed to Phase 3.

Evidence (council): roles=security,code_quality,test_engineer,devil,process_critic | verdict_per_reviewer=security:BLOCK (rubric mismatch), code_quality:APPROVE_WITH_CONDITIONS, test_engineer:APPROVE_WITH_CONDITIONS, devil:APPROVE_WITH_CONDITIONS, process_critic:APPROVE_WITH_CONDITIONS | kills=0 (security BLOCK was rubric-mismatch, not substance) | iterations=1 council round (Step 8 cross-model also 1 iteration) | final=APPROVE_WITH_CONDITIONS, all 6 conditions in D-29..D-34
Evidence (test): spec=EXCEPTION: pure-backend | exception=Rust CLI, no frontend/agentic-tests | result=N/A — TDD enforced in Phase 3 via cargo test | reason=feature is pure-backend; tests defined in plan.md P3-8..P3-11c

---

## Phase 2 Complete — Handoff

### Self-Healing Retrospective

> **SIZE GATE:** Skip for **S** and **M** features — the single retrospective in Phase 4
> (Step 17) covers the entire workflow. Mark all scan boxes as "skipped — S/M feature".

- [ ] Scan 1-4: _(findings or "clean" or "skipped — S/M feature")_
- [ ] All findings fixed

### Update state

```bash
cat > specs/2026-05-04-cli-pretty-tty/FEATURE_STATE.json << 'STATEEOF'
{ "slug": "cli-pretty-tty", "started": "2026-05-04", "mode": "$MODE", "size": "$SIZE",
  "current_phase": 3, "current_step": 12, "last_completed_step": 11,
  "spec_dir": "specs/2026-05-04-cli-pretty-tty", "repo_root": "/Users/j/loom" }
STATEEOF
```

- [ ] `FEATURE_STATE.json` updated to phase 3
- [ ] Cost log: _(total spend this phase)_

**Phase 2 complete.** The orchestrator will now load `phase3-build.md`.
