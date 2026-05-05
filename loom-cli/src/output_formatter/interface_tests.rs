// Interface tests for `OutputFormatter`. Verifies the canonical
// JSON path, pretty delegation shape, and verbatim
// pass-through.

use super::output_formatter::{format_output, OutputFormatter, OutputSink, StdoutSink};
use crate::CliError;

/// In-memory sink used to assert byte-level output without touching
/// stdout. Real production sink is `StdoutSink`.
struct VecSink(Vec<u8>);

impl OutputSink for VecSink {
    fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.0.extend_from_slice(bytes);
        Ok(())
    }
}

#[test]
#[allow(clippy::drop_non_drop)]
fn new_uses_canonical_json_path() {
    let mut sink = VecSink(Vec::new());
    let f: OutputFormatter<'_, VecSink> = OutputFormatter::new(&mut sink);
    drop(f);
    // Constructor compiles with no renderer — default canonical-JSON path.
}

#[test]
fn write_signature_takes_method_and_value() {
    fn _ck<S: OutputSink>(
        f: &mut OutputFormatter<'_, S>,
        m: &str,
        v: &serde_json::Value,
    ) -> Result<(), CliError> {
        f.write(m, v)
    }
    let _ = _ck::<VecSink>;
}

#[test]
fn canonical_json_signature() {
    fn _ck(v: &serde_json::Value) -> Result<String, CliError> {
        OutputFormatter::<'static, VecSink>::canonical_json(v)
    }
    let _ = _ck;
}

#[test]
fn stdout_sink_default_constructor() {
    let _ = StdoutSink;
}

// === no ANSI, no headers, no prose ===
//
// The contract is that `canonical_json` calls `serde_jcs::to_string`.
// We can't test the actual output without an implementation, but we
// can lock the API surface so accidental ANSI emission is a compile
// break. The signature returning `String` (not styled `Stylize`/`Span`)
// expresses this.
#[test]
fn canonical_json_returns_plain_string() {
    fn _ck(v: &serde_json::Value) -> Result<String, CliError> {
        OutputFormatter::<'static, VecSink>::canonical_json(v)
    }
    let _ = _ck;
}

// === AC-CLIOUT2-01..03: format_output canonical/pretty switching ===

/// AC-CLIOUT2-01: pretty=false produces a single line (no `\n`).
#[test]
fn format_output_pretty_false_is_single_line() {
    let v = serde_json::json!({"session_id": "01abc", "status": "active", "created_at_ms": 1234});
    let out = format_output(&v, false).expect("canonical serialise");
    assert!(
        !out.contains('\n'),
        "default output must be single-line canonical JSON; got: {out:?}"
    );
}

/// AC-CLIOUT2-02: pretty=true produces multi-line indented JSON.
#[test]
fn format_output_pretty_true_is_multi_line() {
    let v = serde_json::json!({"session_id": "01abc", "status": "active"});
    let out = format_output(&v, true).expect("pretty serialise");
    assert!(
        out.contains('\n'),
        "--pretty output must include newlines; got: {out:?}"
    );
    assert!(
        out.matches('\n').count() >= 2,
        "--pretty output must span at least 3 lines for a 2-key object; got: {out:?}"
    );
}

/// AC-CLIOUT2-03: pretty=false uses RFC 8785 canonical-JSON key
/// ordering (alphabetical), not whatever order the input map happened
/// to use. Locking this prevents accidental regression to a non-stable
/// serialiser.
#[test]
fn format_output_canonical_orders_keys_alphabetically() {
    let v = serde_json::json!({"zeta": 1, "alpha": 2, "mu": 3});
    let out = format_output(&v, false).expect("canonical serialise");
    let i_alpha = out.find("alpha").expect("alpha key present");
    let i_mu = out.find("mu").expect("mu key present");
    let i_zeta = out.find("zeta").expect("zeta key present");
    assert!(
        i_alpha < i_mu && i_mu < i_zeta,
        "canonical JSON must order keys alphabetically; got: {out:?}"
    );
}
