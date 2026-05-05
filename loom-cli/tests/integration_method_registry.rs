// D-33: prevent the "forget-me" step where adding a new RPC handler
// without registering a curated renderer silently degrades the user
// experience. Asserts every method emitted by handlers maps to either:
//   1. an entry in the curated registry (best UX — tailored layout), OR
//   2. an explicit "documented as silent" allow-list (no curated renderer
//      needed because the method is intentionally minimal).
//
// Updates: when adding a new method to a handler's `emit_to_stdout(...)`
// call, add it either to the curated registry in
// `pretty_renderer/curated/mod.rs` or to the SILENT_BY_DESIGN list below.

use loom_cli::pretty_renderer::curated::lookup;

/// Methods that emit RPC receipts but intentionally have no curated
/// renderer. They fall through to the schema-driven `PrettyRenderer`
/// fallback (and `--quiet` is silent). Anything not in this list and
/// not in the curated registry triggers a test failure.
const SILENT_BY_DESIGN: &[&str] = &[
    // Currently empty — every receipt-emitting handler has a curated
    // renderer. New methods that intentionally have no renderer go here.
];

/// Methods emitted by handler code via `emit_to_stdout(method, ...)`.
/// Sourced from the SUBCOMMAND_RPC_MAP tables in session_commands /
/// vault_commands / action aliases + ad-hoc methods like `gc.run`,
/// `doctor`, `benchmark`, `session.import`.
const ALL_EMITTED_METHODS: &[&str] = &[
    // session.*
    "session.create",
    "session.inspect",
    "session.list",
    "session.close",
    "session.abort",
    "session.replay",
    "session.diff",
    "session.export",
    "session.validate",
    "session.import", // import_playwright handler maps to this
    // vault.*
    "vault.add",
    "vault.list", // vault list handler maps list_grants → vault.list
    "vault.grant",
    "vault.revoke",
    // web.* (action aliases)
    "web.navigate",
    "web.click",
    "web.type",
    "web.select",
    "web.hover",
    "web.scroll",
    "web.wait",
    "web.screenshot",
    "web.snapshot",
    "web.evaluate",
    // local actions
    "gc.run",
    "doctor",
    "benchmark",
];

#[test]
fn every_emitted_method_has_renderer_or_is_documented_silent() {
    let mut missing: Vec<&str> = Vec::new();
    for method in ALL_EMITTED_METHODS {
        let in_registry = lookup(method).is_some();
        let documented_silent = SILENT_BY_DESIGN.contains(method);
        if !in_registry && !documented_silent {
            missing.push(method);
        }
    }
    assert!(
        missing.is_empty(),
        "the following methods are emitted by handlers but have no curated renderer \
         AND are not in SILENT_BY_DESIGN — add them to one or the other:\n  {}",
        missing.join("\n  ")
    );
}

#[test]
fn silent_by_design_does_not_overlap_registry() {
    // A method should be in EXACTLY one place — registry OR silent list,
    // never both. Catches accidental duplicates.
    for method in SILENT_BY_DESIGN {
        assert!(
            lookup(method).is_none(),
            "method {} is in SILENT_BY_DESIGN but ALSO has a curated renderer; \
             remove from SILENT_BY_DESIGN since the renderer takes precedence",
            method
        );
    }
}
