# Anti-AI Code Review

### Rule 1: Over-Abstraction
**Verdict:** CLEAN
The `CuratedRenderer` trait and registry pattern are justified. The feature requires tailored layouts for ~15 commands. The abstraction centralizes dispatch logic and avoids a massive `match` statement, adhering to the "Three instances" rule.

### Rule 2: Comment Pollution
**Verdict:** CLEAN
Comments explain *why* code exists (referencing ACs and Decision IDs like D-7, D-29) rather than just restating *what* it does. For example, comments in `action_commands.rs` explain the mirroring of success paths to avoid double stdout.

### Rule 3: Defensive Paranoia
**Verdict:** CLEAN
Validation occurs at the system boundary (CLI arguments) via `validate_flags` to reject mutually exclusive flags like `--json` and `--pretty`. Internal code trusts internal code.

### Rule 4: Configuration Theater
**Verdict:** CLEAN
Flags (`--json`, `--pretty`, `--quiet`, `--color`) are the core of the requested feature (AC-TTY-01..04). They are not theoretical; they drive the output mode logic.

### Rule 5: Naming Disease
**Verdict:** CLEAN
Names like `OutputMode`, `ColorChoice`, `CuratedRenderer`, `emit`, and `redact_recursive` describe what the code does or is. `CuratedRenderer` is a domain-standard term defined in the spec.

### Rule 6: Error Swallowing
**Verdict:** FINDING
**File:** `loom-cli/src/action_commands/action_commands.rs` (lines 90-93)
**Issue:** The result of `emit_to_stdout` is explicitly ignored using `let _`. If emitting the receipt fails (e.g., broken pipe), the error is silently discarded. While the function eventually returns the original `scheme_check` error, the failure to output the receipt (which the comment claims mirrors the success path) is hidden.
**Fix:** Handle the result explicitly. If printing is best-effort, log to stderr.
```rust
if let Err(e) = crate::output_formatter::emit_to_stdout(canonical_method, v, cfg, None) {
    // Log to stderr if printing the receipt fails, but don't override the original error
    eprintln!("Warning: failed to print receipt: {}", e);
}
```

### Rule 7: Premature Pattern Application
**Verdict:** CLEAN
The `CuratedRenderer` pattern is applied after identifying ~15 instances of similar rendering logic (receipts for different commands). This meets the "Three instances" threshold.

### Rule 8: God Functions Wearing Abstractions
**Verdict:** CLEAN
The `dispatch` function in `curated/mod.rs` is a coordinator (~50 lines) that handles redaction, rendering, and tail composition. It is not a "God Function" masquerading as a class; it performs necessary orchestration.

### Rule 9: Type Theater
**Verdict:** CLEAN
The `OutputMode` enum (`Quiet`, `Json`, `PrettyCurated`, `PrettyFallback`) makes illegal states unrepresentable (e.g., you cannot be both Quiet and Json simultaneously in the resolved config).

### Rule 10: Test Theater
**Verdict:** CLEAN
Tests verify specific behaviors (byte-exactness, flag precedence, quiet IDs) and would fail if the implementation is incorrect. They mock boundaries (config, input) rather than internals.

### Rule 11: Boilerplate Cascade
**Verdict:** CLEAN
The `CuratedRenderer` trait is used to define the interface, and individual files implement it. This avoids copy-pasting the dispatch logic. The specific rendering logic in each file is unique to that command.

### Rule 12: Import/Dependency Bloat
**Verdict:** CLEAN
Imports like `std::io::IsTerminal`, `serde`, and `clap` are used for TTY detection, JSON handling, and CLI parsing respectively. No unused imports are visible.

### Rule 13: Async/Await Cargo Culting
**Verdict:** CLEAN
`async` is used in command handlers to perform RPC calls (`rpc.call(...).await`), which is actual I/O.

### Rule 14: Logging Noise
**Verdict:** CLEAN
`eprintln!` is used for errors and warnings. The `warn_once` function specifically implements rate-limiting (D-32) to prevent log spam.

### Rule 15: Documentation Padding
**Verdict:** CLEAN
Documentation explains context (e.g., "Legacy entry point preserved for incremental migration") and references external specs, rather than just restating function signatures.

### Rule 6: Error Swallowing (Additional Finding)
**Verdict:** FINDING
**File:** `loom-cli/src/pretty_renderer/curated/mod.rs` (lines 325-328)
**Issue:** The `warn_once` function contains a compilation error. The variable `lock` is used without being defined, and the static `WARNED` is initialized incorrectly (`Mutex::new()` assigned to `OnceLock` type). This is a hallucination/syntax error that prevents compilation.
**Fix:** Correct the static initialization and variable binding.
```rust
fn warn_once(method: &str, err: &CliError) {
    static WARNED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let lock = WARNED.get_or_init(|| Mutex::new(HashSet::new()));
    let mut set = lock.lock().expect("WARNED mutex poisoned");
    // ... rest of function
}
```

## Summary
- Findings: 2/15
- Critical (must fix):
  - `loom-cli/src/pretty_renderer/curated/mod.rs` (Syntax error/undefined variable `lock` in `warn_once`)
- Minor (should fix):
  - `loom-cli/src/action_commands/action_commands.rs` (Ignoring result of `emit_to_stdout` with `let _`)
