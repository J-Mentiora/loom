// Re-export of the locked Phase 5.3 interface tests. DO NOT EDIT here.
// Edit `systems/loom-cli/modules/PrettyRenderer/interface_tests.rs` instead.
// Interface tests for `PrettyRenderer`. Verifies IC-CLI-02
// schema-driven column derivation and NO_COLOR handling.

use super::pretty_renderer::{detect_color_enabled, PrettyRenderer};
use crate::schema_cache::SchemaCache;
use crate::CliError;

#[test]
fn render_signature_takes_method_and_value() {
    fn _ck<'a>(
        r: &'a PrettyRenderer<'a>,
        m: &str,
        v: &serde_json::Value,
    ) -> Result<String, CliError> {
        r.render(m, v)
    }
    let _ = _ck;
}

#[test]
fn columns_from_schema_signature() {
    fn _ck(s: &serde_json::Value) -> Vec<String> {
        PrettyRenderer::columns_from_schema(s)
    }
    let _ = _ck;
}

#[test]
fn with_color_constructor_takes_bool() {
    fn _ck(s: &SchemaCache, c: bool) -> PrettyRenderer<'_> {
        PrettyRenderer::with_color(s, c)
    }
    let _ = _ck;
}

#[test]
fn detect_color_enabled_signature() {
    fn _ck() -> bool {
        detect_color_enabled()
    }
    let _ = _ck;
}

// === IC-CLI-02: schema-driven column list ===
//
// `columns_from_schema` is a pure function over a JSON-Schema object;
// the test below documents the input shape (a JSON Schema with
// `properties` map). The runtime invariant is that columns are derived
// from `properties` order; verified via implementation tests in 5.4.
#[test]
fn columns_from_schema_signature_lock() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "session_id": {"type": "string"},
            "status": {"type": "string"},
        }
    });
    let _ = PrettyRenderer::columns_from_schema(&schema);
    // Phase 5.4 will assert byte-equal column ordering. For now the
    // signature is the contract surface.
}

// === D-16 NO_COLOR spec correctness ===
//
// Per https://no-color.org: "NO_COLOR... when present and not an empty
// string". The earlier `is_ok()` check treated empty string as a disable
// signal, which the spec explicitly rejects.
//
// Tests serialise on a process-wide mutex because env vars are per-process.

use std::sync::Mutex;
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn no_color_unset_allows_color() {
    let _g = ENV_LOCK.lock().unwrap();
    std::env::remove_var("NO_COLOR");
    std::env::remove_var("TERM");
    assert!(detect_color_enabled());
}

#[test]
fn no_color_empty_does_not_disable() {
    let _g = ENV_LOCK.lock().unwrap();
    std::env::set_var("NO_COLOR", "");
    std::env::remove_var("TERM");
    // Per spec: empty value is NOT a disable signal.
    assert!(detect_color_enabled());
    std::env::remove_var("NO_COLOR");
}

#[test]
fn no_color_non_empty_disables() {
    let _g = ENV_LOCK.lock().unwrap();
    std::env::set_var("NO_COLOR", "1");
    std::env::remove_var("TERM");
    assert!(!detect_color_enabled());
    std::env::remove_var("NO_COLOR");
}

#[test]
fn term_dumb_disables_color() {
    let _g = ENV_LOCK.lock().unwrap();
    std::env::remove_var("NO_COLOR");
    std::env::set_var("TERM", "dumb");
    assert!(!detect_color_enabled());
    std::env::remove_var("TERM");
}
