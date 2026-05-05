# Deep Codebase Scan: loom-cli-pretty-tty Feature

**Scan date:** 2026-05-04 | **Mode:** BUILD | **Size:** M | **Codebase:** /Users/j/loom-cli-pretty-tty

---

## A. Receipt Shapes per Command

Compiled from embedded schemas in `/Users/j/loom-cli-pretty-tty/loom-cli/src/postinstall_runner/postinstall_runner.rs:~220` and handler signatures.

| Command | RPC Method | Primary ID Field | Top-Level Receipt Fields | Peculiar Shapes | Notes |
|---------|------------|------------------|-------------------------|-----------------|-------|
| `action web.navigate` | `web.navigate` | `action_hash` | action_id, session_id, status, timing_ticks, action_hash, outcome_hash, emitted_at_ms, url, final_url, title, status_code, dom_snapshot_hash, screenshot_after_hash, console_count, network_summary | network_summary: {total_count, total_bytes, error_count}; console_lines: array | Navigate tier-2 wire receipt (AC-NAVRECEIPT2-01) |
| `action web.click/type/select/hover/scroll/wait` | `web.click` etc | `action_hash` | action_hash, outcome_hash, emitted_at_ms | None | Hash-only tier (minimal) |
| `action web.evaluate` | `web.evaluate` | `action_hash` | action_hash, outcome_hash, emitted_at_ms, return_value_json, return_value_blob_ref | return_value_blob_ref: {sha256, size_bytes} when JSON > 64KB | AC-EVALRESULT-01..04 |
| `action web.screenshot/snapshot` | `web.screenshot` | `action_hash` | action_hash, outcome_hash, emitted_at_ms | None | Hash-only tier |
| `session create` | `session.create` | `session_id` | session_id + metadata | None | RPC returns full SessionInfo |
| `session inspect` | `session.inspect` | `session_id` | manifest_summary (extracted subfield) | None | Handler extracts .manifest_summary (session_commands.rs:342) |
| `session list` | `session.list` | — | array of sessions | None | Returns list of session objects |
| `session close/abort/replay/validate` | `session.close` etc | `session_id` | — | status: "error" possible | — |
| `session diff` | `session.diff` | — | diff (extracted subfield) | field_diffs: array, action_count_delta: int | Handler extracts .diff (session_commands.rs:431) |
| `session export` | `session.export` | artifact_ref | artifact_ref | None | Two-stage RPC: export → content.get (binary output) |
| `vault grant/revoke/list/add` | `vault.grant` etc | grant_id | — | None | Inferred from handler signature |
| `gc` | `gc.run` | — | status, deleted_count | None | — |
| `doctor` | local action | — | DoctorReport | status, checks: array | Emitted via serde_json::to_string (command_router.rs:216) **NOT via format_output** |
| `import playwright` | `session.import` | session_id | — | None | — |
| `benchmark` | local action | — | BenchmarkReport | status, results: object | Emitted via format_output (benchmark_commands.rs:45) |

**Carve-outs (not yet unified):**
- **doctor:** Direct serde_json::to_string (command_router.rs:216) — will need migration
- **serve/postinstall/version:** Local actions with custom receipt types

---

## B. Refactoring Opportunities

### B1. format_output Callsite Uniformity

**Pattern: ALL 18 tracked callsites use identical pattern:**
```rust
println!("{}", format_output(&resp, cfg.pretty)?)
```

**Exceptions (field extraction):**
1. **session inspect** (session_commands.rs:340–350): Extracts `.manifest_summary`
2. **session diff** (session_commands.rs:430–437): Extracts `.diff`
3. **session export** (session_commands.rs:474–499): Two-stage RPC chain (not a single receipt)

**Migration impact:** Can replace `format_output` in-place for 15 uniform callsites. The 3 exceptions need custom emit functions or a receipt-wrapper pattern.

### B2. Receipt Structs vs. serde_json::Value

**Finding: ALL handlers use `serde_json::Value` for RPC responses.** No declarative Receipt types in loom-cli.

Field-extracting handlers deserialize into local structs:
```rust
#[derive(serde::Deserialize)]
struct SessionInspection { manifest_summary: serde_json::Value }
```

**Implication:** Curated renderers work entirely on `serde_json::Value`. Fallback and unrendered-field logic must be generic over shape.

### B3. cfg.pretty Migration Surface

**Current readers:** 16 locations across 6 modules:
- `cli_config.rs:39` (field definition), `105`, `164`, `238–245` (resolution)
- `command_router.rs:39`, `46` (CLI flag and flow)
- `action_commands.rs:102`, `session_commands.rs:325/349/357/368/385/413/437`, `vault_commands.rs:~100`, `admin_commands.rs:69`, `benchmark_commands.rs:45`

**Migration:** Change `cfg.pretty: bool` to `cfg.output_mode: OutputMode` enum (Quiet/Json/PrettyCurated/PrettyFallback). Parser in apply_overrides (cli_config.rs:238) updates to parse enum variants.

---

## C. Test Infrastructure

### C1. Subprocess vs. In-Process Tests

**Two patterns:**
1. **Subprocess tests** (integration_naverr_cli_e2e.rs): Spawn loom binary; stdout is non-TTY
   - `IsTerminal` returns false → auto-detect never triggers pretty rendering
2. **In-process tests** (cli_integration.rs:28–56): Call loom_cli::run() library function
   - Example: test_ac_cli_04_1_canonical_json_parseable (line 29)

**Implication:** `--pretty` is the only way to enable pretty rendering in CI; auto-detect will not work on subprocess stdout.

### C2. Snapshot/Golden-File Testing

**Finding: NO snapshot crate (insta) used.** Test pattern:
- Assertions on parsed JSON via serde_json::from_str + field comparison
- No .snap fixtures

**Fixture location:** Create `loom-cli/tests/fixtures/` or `loom-cli/tests/snapshots/` for golden-file output (new convention needed).

### C3. Mocks Module

**File:** `/Users/j/loom-cli-pretty-tty/loom-cli/src/mocks.rs`
- Contains only assertion stubs (MockCommandRouter, MockRpcClient)
- NO canned RPC responses

**Real test RPC:** Python SDK (conftest.py) implements MockDaemon listening on Unix socket with framed loom-rpc protocol.

---

## D. Known Traps

### D1. Output Byte Stability

**Existing test:** cli_integration.rs:28–43
- Parses output to verify JSON semantics
- Asserts no ESC byte (`\x1b`)

**Trap:** New emit() dispatcher must preserve byte-for-byte canonical JSON on default path. AC-TTY-02 regression test (per decisions.md) is new—add to interface_tests.rs.

### D2. Clippy Lints

**Forbidden patterns:**
- `// FORBIDDEN: std::process::exit outside main + ErrorMapper` (error_mapper.rs + cli_main/interface_tests.rs)
- Clippy bans `Receipt::redact` calls in handler modules (session_commands.rs:16)
- Clippy bans hand-rolled column lists (pretty_renderer.rs:10)

**No explicit lint against to_string_pretty outside output_formatter**, but enforced by code structure. Verify in clippy.toml / deny.toml if needed.

### D3. Interface Tests Pinning format_output

**Tests in output_formatter/interface_tests.rs:70–113:**
- format_output_pretty_false_is_single_line (line 74)
- format_output_pretty_true_is_multi_line (line 85)
- format_output_canonical_orders_keys_alphabetically (line 103)

**These remain valid after migration.** They verify the fallback path when no curated renderer exists.

### D4. CI Gates

**Justfile:** Only gen-meta for binary-size benchmark (no output-format gates)
**Python SDK tests:** Parse JSON responses; safe from pretty-format regressions

---

## E. Cross-System Impact

### E1. External Consumers

**loom-mcp** (loom-mcp/src/mcp_main/mod.rs):
- Spawns loom as subprocess
- Reads stdout JSON
- **Action:** Must pass `--json` flag to prevent pretty rendering in MCP context

**python-sdk:** Uses Unix socket RPC to daemon, not CLI subprocess → **No impact**

**typescript-sdk:** Not found in workspace

### E2. AC-TTY-02 Coverage

**Add regression tests:**
1. **Unit test** (output_formatter/interface_tests.rs):
   ```rust
   #[test]
   fn ac_tty_02_default_path_is_byte_exact() {
       // emit(receipt, "web.navigate", cfg_quiet_json_pretty_all_false)
       // Assert output == serde_jcs::to_string(receipt) + "\n"
   }
   ```
2. **Subprocess test** (cli_integration.rs):
   ```rust
   #[test]
   fn ac_tty_02_subprocess_stdout_not_tty_stays_canonical() {
       // Spawn loom as subprocess (stdout not TTY)
       // No flags (cfg.pretty defaults to false)
       // Assert output is single-line canonical JSON
   }
   ```

---

## Summary: 3 Most Consequential Findings

### 1. **Field Extraction Pattern (BLOCKER)**
`session.inspect` and `session.diff` extract `.manifest_summary` and `.diff` subfields **before** calling format_output. New emit() cannot blindly replace format_output—must either:
- Accept optional `projection_field: &str` parameter, OR
- Move projection into dispatcher post-RPC deserialization, OR
- Revert to full RPC response and let dispatcher handle schema-driven extraction

**Recommendation:** Move projections into emit() as post-RPC step; schema metadata hints which field to extract.

### 2. **doctor Command Carve-Out (BLOCKER)**
doctor (command_router.rs:216) uses `serde_json::to_string()` directly, **not** format_output. Explicit migration required. Current code bypasses cfg.pretty entirely.

### 3. **NO_COLOR Bug in pretty_renderer.rs:101 (TRAP)**
`std::env::var("NO_COLOR").is_ok()` treats empty string as "enabled" (spec violation). Per no-color.org, empty string means disabled.

**Fix needed before launch:**
```rust
// Current (bug):
if std::env::var("NO_COLOR").is_ok() { return false; }

// Correct logic:
match std::env::var("NO_COLOR") {
    Err(_) => true,  // not set → allow color
    Ok(s) if s.is_empty() => false, // set to "" → disable color  
    Ok(_) => false, // set to anything else → disable color
}
```

---

**Critical file paths:**
- Receipt schemas: loom-cli/src/postinstall_runner/postinstall_runner.rs:~220
- format_output callsites: action_commands.rs:102, session_commands.rs:325/349/357/368/385/413/437, vault_commands.rs:~100, admin_commands.rs:69, benchmark_commands.rs:45
- doctor carve-out: command_router.rs:216
- NO_COLOR bug: pretty_renderer.rs:101
- Field projections: session_commands.rs:340–350, 430–437
- Config migration: cli_config.rs:238–245, command_router.rs:39,46
- Interface tests: output_formatter/interface_tests.rs:70–113, cli_integration.rs:28–80
