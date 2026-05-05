# Ambiguity Detection Report

### Finding 1
- **Category:** 3 — Implicit assumptions
- **Location:** Step P3-5 — "Tail block format... in JSON-Schema property order from `SchemaCache` if available, else map iteration order."
- **Problem:** "Map iteration order" in Rust (typically `HashMap`) is non-deterministic (randomized per process). If `SchemaCache` is unavailable or incomplete, the tail block content will vary on every run, breaking the "golden tests" (Step P3-9) and reproducibility requirements.
- **Suggested resolution:** "If SchemaCache is unavailable, sort keys alphabetically to ensure deterministic output for the tail block."

### Finding 2
- **Category:** 3 — Implicit assumptions
- **Location:** Step P3-6 — "`session.list`: table-style — header row dim, one row per session (id, status, created_at). 80-col safe."
- **Problem:** "Table-style" and "80-col safe" do not define column widths, alignment (left/right), or wrapping behavior for long fields (e.g., ISO8601 timestamps or long IDs). Implementers may guess, leading to inconsistent UI.
- **Suggested resolution:** "Define column widths (e.g., ID=40 chars, Status=10, Date=30) and overflow behavior (e.g., truncate with '...' or wrap to next line)."

### Finding 3
- **Category:** 1 — Vague verbs
- **Location:** Step P3-6 — "`session.diff`... summary line + per-diff lines + tail."
- **Problem:** "Per-diff lines" does not specify the visual syntax of the diff. Should it use unified diff format (`-`/`+`), key-value arrows (`key: old -> new`), or another format?
- **Suggested resolution:** "Specify diff format: e.g., `<DIM>- key: old_value</DIM>` and `<GREEN>+ key: new_value</GREEN>`."

### Finding 4
- **Category:** 5 — Missing error paths
- **Location:** Step P3-5 — "If no curated renderer exists → return `Err(NotFound)` so `emit` falls through..."
- **Problem:** The plan handles the "missing renderer" case but not the "renderer exists but fails" case. If a curated renderer panics or returns an `Err` (e.g., due to unexpected data shape), the system behavior is undefined.
- **Suggested resolution:** "If a curated renderer returns an Error, fall back to `PrettyFallback` or `Json` mode with a warning log, rather than crashing."

### Finding 5
- **Category:** 5 — Missing error paths
- **Location:** Step P3-2 — "Update all 18 callsites to `emit(method, &resp, cfg)`."
- **Problem:** The signature of `emit` is `Result<String, CliError>`. The plan does not specify how the 18 callsites should handle this result. Should they unwrap, panic, or propagate the error?
- **Suggested resolution:** "Specify that callsites must propagate the error or use `expect` with a descriptive message if formatting failure is considered unrecoverable."

### Finding 6
- **Category:** 3 — Implicit assumptions
- **Location:** Step P3-6 — "`web.navigate`... `network_summary: total=X bytes=Y errors=Z`."
- **Problem:** The template implies static labels ("bytes", "errors"). It does not account for singular/plural grammar (e.g., "1 error" vs "2 errors"), which looks unprofessional.
- **Suggested resolution:** "Specify pluralization logic: use 'byte'/'error' if count is 1, 'bytes'/'errors' otherwise."

## Summary
- Total findings: 6
- By category: {1: 1, 2: 0, 3: 3, 4: 0, 5: 2}
- Critical (blocks implementation): 
  - Finding 1 (Non-deterministic tail block order breaks tests)
  - Finding 4 (Undefined behavior on renderer failure)
  - Finding 5 (Undefined error handling at callsites)
- Advisory (could cause confusion): 
  - Finding 2 (Table layout specifics)
  - Finding 3 (Diff format syntax)
  - Finding 6 (Grammar/pluralization)
