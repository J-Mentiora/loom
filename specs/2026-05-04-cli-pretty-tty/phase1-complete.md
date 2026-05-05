# Pretty TTY Output — Phase 1: Discovery

Progress tracker for Steps 0–3. Tick boxes and fill evidence after EVERY step.
Save this file to disk after each step.

> **Slug**: `cli-pretty-tty` | **Branch**: `cli-pretty-tty` | **Started**: 2026-05-04
> **Spec folder**: `specs/2026-05-04-cli-pretty-tty/` | **Mode**: _(Step 0)_ | **Size**: _(Step 0)_

**RULES:** (1) MANDATORY STOP = present output, WAIT for response. (2) Questions use
`AskUserQuestion` with 2-4 concrete options. (3) One step at a time — complete, update
file, then next. (4) BLOCKING = council KILL, gate failure ×3, policy violation.
(5) Artifacts go in `specs/2026-05-04-cli-pretty-tty/`. (6) Update `FEATURE_STATE.json` after each step.
(7) Log OpenRouter calls to `specs/2026-05-04-cli-pretty-tty/cost-log.jsonl`. (8) **Telemetry**: record
`step_start` timestamp (ISO 8601) at the start of each step in a `specs/2026-05-04-cli-pretty-tty/timings.jsonl`
file (one JSON object per line: `{"step": N, "phase": 1, "event": "start|end", "ts": "..."}`).
Record `step_end` when the step is complete.

### Size Guide

| Size | Description | Steps affected |
|------|-------------|----------------|
| **S** | Well-defined, single-file or few-file change. No ambiguity. | Skip Step 2 (light scan), skip domain presets, 0-1 Qs in Step 3 |
| **M** | Multi-file feature, some design decisions needed. | Full workflow, combined stops where noted |
| **L** | Cross-cutting, multi-system, or high-risk feature. | Full workflow, all stops enforced individually |

---

## Step 0: Pre-flight

**Model check:** Verify model is **Opus** via system context. If not Opus, STOP.

**Effort:** All sub-agents needing deep reasoning MUST use `model: "opus"`.

Verify you are in the worktree on the correct branch.

```bash
git config core.hooksPath .githooks
```

### Resume check

Look for `specs/2026-05-04-cli-pretty-tty/FEATURE_STATE.json`. If found: display progress, ask user via
`AskUserQuestion` (A: Resume / B: Start fresh / C: Abort). If resuming: mark prior steps
`[x] RESUMED`, read checkpoint/phase output files, fast-forward to `current_step`.

### Size assessment

Assess size using signals below. State chosen size and move on — no user confirmation needed.

| Signal | S | M | L |
|--------|---|---|---|
| Files touched | 1-3 | 4-10 | 10+ |
| New DB tables/columns | 0 | 0-2 | 3+ |
| New API endpoints | 0-1 | 2-4 | 5+ |
| Design ambiguity | None | Some | Significant |
| Security surface | None | Minor | Auth/PII/billing |
| Cross-system | No | Maybe | Yes |

### Mode detection

Auto-detect: **BUILD** ("add", "create", "implement", "new", "redesign") or
**FIX** ("fix", "bug", "broken", "regression", "error", "crash"). Ambiguous → ask user.

### Save state

```bash
cat > specs/2026-05-04-cli-pretty-tty/FEATURE_STATE.json << 'STATEEOF'
{ "slug": "cli-pretty-tty", "started": "2026-05-04", "mode": "$MODE", "size": "$SIZE",
  "current_phase": 1, "current_step": 1, "last_completed_step": 0,
  "spec_dir": "specs/2026-05-04-cli-pretty-tty", "repo_root": "/Users/j/loom" }
STATEEOF
```

- [x] Model is **Opus** (4.7, 1M context)
- [x] Hooks configured (`core.hooksPath=.githooks`)
- [x] In worktree on branch `cli-pretty-tty`
- [x] Resume check: fresh start (no prior state)
- [x] Mode: **BUILD** (verb "Add" → BUILD)
- [x] Size: **S** (3-4h estimate; CLI plumbing + golden-file tests; tooling category; no DB / no new endpoints / no security surface)
- [x] `FEATURE_STATE.json` written

Evidence: model=opus-4-7 | dir=/Users/j/loom-cli-pretty-tty | branch=cli-pretty-tty | resume=fresh | mode=BUILD | size=S

---

## Step 1: Load Context

- [ ] Read `invariants/features.md`. Identify the feature to build.
- [ ] If feature in `code_specs/080-cx-roadmap/prd.md`, read relevant sections.

### Context routing

Pick the **single best category** for which invariant docs to load:

| Category | Load |
|----------|------|
| **frontend** | `code-quality.md`, `security.md`, `user-experience.md` + **`mentiora-product-ux` skill** (see below) |
| **backend** | `code-quality.md`, `security.md`, `architecture.md`, `doc/policies/secure-development-policy.md` |
| **fullstack** | `code-quality.md`, `security.md`, `user-experience.md`, `architecture.md`, `doc/policies/secure-development-policy.md` + **`mentiora-product-ux` skill** (see below) |
| **infra** | `security.md`, `architecture.md`, `doc/policies/secure-development-policy.md` |
| **tooling** | `code-quality.md` |

Add `testing.md` if the feature involves tests.

**Design guide (frontend/fullstack only):** The design guide is the `mentiora-product-ux`
skill installed at `~/.claude/skills/mentiora-product-ux/`. Read its `SKILL.md` for the
index, then read the relevant `docs/*.md` files for the current task. If the skill is not
installed, tell the developer to run:
```
git clone git@github.com:mentiora-ai/mentiora-product-ux.git ~/.claude/skills/mentiora-product-ux
```

⚡ **PARALLEL:** Read all routed docs in a single batch.

### Domain preset (M/L only)

> **SIZE GATE:** Skip for **S**.

Select domain preset — pre-populates council roles in Phase 2:

| Preset | Auto-include roles | Weight boost |
|--------|-------------------|--------------|
| **cx-platform** | `cx_domain`, `end_user`, `accessibility` | UX, domain fidelity |
| **integrations** | `devops`, `performance`, `cx_domain` | reliability, latency |
| **auth-security** | `security`, `privacy`, `legal` | security, compliance |
| **data-pipeline** | `performance`, `devops`, `privacy` | data integrity, latency |
| **ui-ux** | `end_user`, `accessibility`, `b2b_*` | UX, accessibility |
| **api** | `performance`, `security`, `devops` | reliability, security |
| **tooling** | `process_critic`, `code_quality` | maintainability, DX |
| **custom** | _(specify)_ | _(specify)_ |

- [x] Category selected: **tooling** (CLI / loom-cli crate)
- [x] Routed docs loaded — no `invariants/` or `code_specs/` exist in this repo; loaded
  in-tree contracts instead: `loom-cli/src/output_formatter/`, `pretty_renderer/`,
  `cli_config/`, `command_router/`, `session_commands/`, `action_commands/` and their
  `interface_tests.rs`. IC-CLI-01 (canonical JSON default), IC-CLI-02 (`--pretty` →
  PrettyRenderer), SR-CLI-03 (receipt pass-through), AC-CLIOUT2-03 (`cfg.pretty`
  mirroring) all read.
- [x] Domain preset selected (M/L): N/A — size **S**
- [x] Asked via `AskUserQuestion` (4 + 1 follow-up Qs)
- [x] **Domain preset (M)**: `tooling` (process_critic, code_quality)
- [x] **Size revised**: S → M after Step 3 Q1 (curated layouts for all subcommands)

Evidence: feature=pretty-TTY-output-loom-cli | category=tooling | docs=in-tree contracts (IC-CLI-01/02, SR-CLI-03, AC-CLIOUT2-03) | preset=tooling | Q&A=Step 3

---

## Step 2: Light Codebase Scan

> **SIZE GATE:** Skip for **S** features (S gets deep scan in Phase 2).

**ISOLATION RULE:** Run as `Agent` tool (`subagent_type: "Explore"`, thoroughness "medium").

⚡ **PARALLEL:** Can run in background while presenting Step 1's AskUserQuestion.

Launch Explore agent with mode-appropriate mission:

**BUILD**: Find existing patterns, reusable components, similar precedents (5-10 files).
**FIX**: Find affected code paths, related tests, recent changes.

- [x] Explore agent launched (NOT in main context) — completed under M after size bump
- [x] Report saved to `specs/2026-05-04-cli-pretty-tty/codebase-scan-light.md`
- [x] Key findings summarized

Evidence: scan_mode=BUILD | findings_count=21 stdout sites + 2 stderr + 6 test pin files | top_3=(1) format_output is single choke-point at 18/21 sites; (2) existing --pretty is indented JSON only, schema renderer wired but unreached; (3) TTY detection is greenfield — std::io::IsTerminal at MSRV 1.92.

---

## Step 3: Clarifying Questions (Round 1)

> **SIZE GATE:** S: 0-1 Qs, skip if clear. M: 1-2 Qs mandatory. L: 2+ Qs, **MANDATORY STOP**.

Focus: clarify scope, confirm understanding, identify biggest unknown.
Use `AskUserQuestion` with concrete options. One question per call, up to 2 calls.

- [x] Questions asked (count): 4 + 1 follow-up = **5**
- [x] Key answers recorded in `discovery-output.md` § Round 1 Q&A

Evidence: questions=5 | answers=(Q1) repurpose --pretty = human; (Q2) tailored projections everywhere; (Q3) --quiet prints session_id for create / action_hash for action; (Q4) hand-rolled ANSI no dep; (Q5/follow-up) bump size to M | remaining_unknowns=none

---

## Phase 1 Complete — Handoff

### Self-Healing Retrospective

> **SIZE GATE:** Skip for **S** and **M** features — the single retrospective in Phase 4
> (Step 17) covers the entire workflow. Mark all scan boxes as "skipped — S/M feature"
> and proceed directly to "Write discovery output".

1. **Workarounds** — hardcoded values, `any` types, inline hacks?
2. **Silent failures** — ignored warnings, vacuous tests, suppressed lints?
3. **Skipped steps** — items skipped without SIZE/MODE GATE exception?
4. **Corner-cuts** — items marked done without verification?

- [x] Scan 1 — Workarounds: skipped — M feature (deferred to Phase 4 Step 17)
- [x] Scan 2 — Silent failures: skipped — M feature
- [x] Scan 3 — Skipped steps: skipped — M feature
- [x] Scan 4 — Corner-cuts: skipped — M feature
- [x] All findings fixed: skipped — M feature

### Write discovery output

Write `specs/2026-05-04-cli-pretty-tty/discovery-output.md`: Mode, Size, Feature (1-line), Context category,
Domain preset + roles, Light scan summary, Invariant docs loaded, Open questions, Round 1 Q&A.

### Update state

```bash
cat > specs/2026-05-04-cli-pretty-tty/FEATURE_STATE.json << 'STATEEOF'
{ "slug": "cli-pretty-tty", "started": "2026-05-04", "mode": "$MODE", "size": "$SIZE",
  "current_phase": 2, "current_step": 4, "last_completed_step": 3,
  "spec_dir": "specs/2026-05-04-cli-pretty-tty", "repo_root": "/Users/j/loom" }
STATEEOF
```

- [x] `discovery-output.md` written
- [x] `FEATURE_STATE.json` updated to phase 2
- [x] Cost log: $0 (no OpenRouter calls in Phase 1)

**Phase 1 complete.** The orchestrator will now load `phase2-design.md`.
